//! 真正搬存档的那几个 RPC 与会话钩子：测试连接、立即同步、列版本、恢复，以及
//! 启动前取回和退出后上传。
//!
//! 与 `credentials.rs` 分开：这里每一次调用都会碰网络与用户的存档，所以每一条
//! 都必须自带"失败时说什么"。

use serde_json::{Value, json};

use crate::config::{SyncConfig, SyncEngine};
use crate::secrets::Keyring;

use super::{CHECK_TIMEOUT, Daemon, GameOutcome, PULL_TIMEOUT, SETTLE_DELAY, sync};

/// 「恢复」自己的总上限：它要下载、解包、再铺回本机，比"点开看一眼"慢得多 ——
/// 网络卡住时用户不该无限等，而正常的一次恢复也绝不能被误杀（BUG-12）。
const RESTORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 同步现在为什么跑不起来；一切就绪时是 `None`。
///
/// 顺序是"先问最根本的"：配置本身写错了吗 → 凭据在不在 → 当前引擎那个程序找得到吗。
/// 最后一步要分成两种说法，因为它们完全不同：**你填的位置不对**
/// （[`sync::misconfigured`]，用户明明填了 D 盘却被回一句"PATH 里找不到"是答非所问）
/// 和**根本没装**（那就该给出安装命令）。
///
/// `engine_binary` 是当前引擎那个可执行文件（找到了就是它的路径）。它只依赖参数，
/// 所以不用起 daemon 也能测。
pub(super) fn readiness_problem(
    settings: &SyncConfig,
    keyring: &Keyring,
    engine_binary: Option<&str>,
) -> Option<String> {
    sync::validate(settings)
        .err()
        .or_else(|| sync::validate_secrets(keyring).err())
        .map(|error| error.to_string())
        .or_else(|| {
            let (configured, name) = match settings.engine {
                SyncEngine::Rclone => (&settings.rclone_binary, "rclone"),
                SyncEngine::Kopia => (&settings.kopia_binary, "kopia"),
            };
            sync::misconfigured(configured, name).or_else(|| {
                // 点明是哪一个 —— 两个引擎互为备选，用户很可能只装了一个。
                engine_binary.is_none().then(|| match settings.engine {
                    SyncEngine::Rclone => {
                        "找不到 rclone：在设置页填上它的位置（目录或完整路径都行），\
                         或者 Arch: sudo pacman -S rclone"
                            .to_string()
                    }
                    SyncEngine::Kopia => {
                        "找不到 kopia：在设置页填上它的位置（目录或完整路径都行），\
                         或者 Arch: sudo pacman -S archlinuxcn/kopia"
                            .to_string()
                    }
                })
            })
        })
}

impl Daemon {
    /// Check credentials, bucket and write access.
    ///
    /// 「测试连接」是个按钮,用户盯着它等 —— 所以它**必须**在有限时间内给出答案。
    /// 底下那三步(kopia 连桶、必要时建仓库、列一次快照)各自的上限是 300 秒的
    /// [`CHECK_TIMEOUT`] 之外的 [`crate::sync::runner::COMMAND_TIMEOUT`],叠起来能把
    /// 按钮灰着转十几分钟;界面那边只会显示一句"正在测试连接…",跟卡住没区别
    /// (用户 2026-09-18 报的"点了没反应")。这里给它一个**明显短于一次真实同步**的
    /// 总预算,到点如实说超时。
    pub(in crate::daemon) async fn rpc_sync_test(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let remote = tokio::time::timeout(CHECK_TIMEOUT, runner.check())
            .await
            .map_err(|_| {
                format!(
                    "测试连接超过 {} 秒没有回应 —— 桶名/端点/凭据对不对?网络通不通?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "remote": remote }))
    }

    /// Upload now: one game, or every game that has save locations.
    pub(in crate::daemon) async fn rpc_sync_now(
        &self,
        game_id: Option<&str>,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;

        let ids: Vec<String> = match game_id {
            Some(id) => vec![id.to_string()],
            None => self
                .config
                .read()
                .await
                .games
                .iter()
                .filter(|(_, game)| !game.save_paths.is_empty())
                .map(|(id, _)| id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return Err("还没有任何游戏配置了存档位置".to_string());
        }

        let mut outcomes = Vec::with_capacity(ids.len());
        for id in ids {
            let (name, targets) = match self.sync_targets(&id).await {
                Ok(pair) => pair,
                Err(error) => {
                    outcomes.push(GameOutcome::failed(&id, &id, error));
                    continue;
                }
            };
            // 身份在这一刻定下来（第一次上传 = 这台机器认领这一款），并写进包里 ——
            // 取回之前比的就是它（见 `crate::sync::cloud::identity_match`）。
            let packed = match self.pack_identity(&runner, &id, &name, &targets).await {
                Ok(packed) => packed,
                Err(error) => {
                    outcomes.push(GameOutcome::failed(&id, &name, error));
                    continue;
                }
            };
            let outcome = runner
                .upload(
                    &id,
                    &name,
                    &packed.cloud_key,
                    &targets,
                    Some(&packed.identity),
                )
                .await;
            self.sync.remember(&id, "上传", &outcome);
            if outcome.ok {
                // 索引：**尽力而为**（写坏了不影响"这一版已经上去了"这件事）。
                self.refresh_index_for(&runner, &id).await;
            }
            outcomes.push(outcome);
        }

        Ok(json!({
            "ok": outcomes.iter().all(|o| o.ok),
            "games": outcomes,
        }))
    }

    /// The version packages the cloud holds for a game.
    ///
    /// 收的是**本机游戏 id**，但版本是按**云端落点**放的：第一次上传时身份一建，
    /// 落点就不再等于本机 id（kopia 那边落点就是身份 uuid），所以这里必须先把 id
    /// 解析成落点。从前直接把 id 当落点用，只有"这一款在云端正好落在同名目录里"
    /// 时才碰对地方 —— 那正是 rclone 上的巧合，而 kopia 上列出来永远是空的
    /// （PLATFORMS 2026-09-24 记的那条未定性现象）。
    pub(in crate::daemon) async fn rpc_sync_versions(
        &self,
        game_id: &str,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        // 关着的时候直接说清楚：从前这里照样构造 runner、照样发远端调用，用户看到的
        // 是"等半天然后一个网络错误"，分不清是没开还是网不通（BUG-12）。
        if !settings.enabled {
            return Err("云同步未启用".to_string());
        }
        // 本机配置里有这一款时，按记下来的**云端落点**问：落点不一定等于本机 id ——
        // 第一次上传时身份一建，落点就不再等于 id（kopia 那边落点就是身份 uuid），
        // 从前直接拿 id 当落点用，kopia 上永远列不到（PLATFORMS 2026-09-24 那条
        // 未定性现象就是它）。配置里没有这一款时（"云端有、本机没有"的那类）保持
        // 老行为：把传进来的字符串当落点试一次，别把这条路堵死。
        let cloud_key = match self.cloud_key_of(game_id).await {
            Ok(key) => key,
            Err(_) => game_id.to_string(),
        };
        let runner = self.sync_runner(&settings)?;
        let versions = runner
            .packages(&cloud_key)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "versions": versions }))
    }

    /// 云端某一款有哪几版 —— 按**云端落点**（`cloud_dir`，也就是
    /// [`Self::rpc_sync_cloud_games`] 给的 `id`）列，不按本机 id。
    ///
    /// 这两件事在多数机器上恰好同名，于是[上面那条](Self::rpc_sync_versions)的错位
    /// 一直没显形；但"跟着对方目录走"的那台机器上它们不同名，而**云端有、本机没有**
    /// 的游戏根本不会出现在本机 id 里 —— 「云端存档」这一块要列的正是这些。
    pub(in crate::daemon) async fn rpc_sync_cloud_versions(
        &self,
        cloud_key: &str,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        // 与「刷新云端清单」同样的理由给一个上限：点开一款是一次网络往返，用户盯着等。
        let versions = tokio::time::timeout(CHECK_TIMEOUT, runner.version_infos(cloud_key))
            .await
            .map_err(|_| {
                format!(
                    "列《{cloud_key}》的版本超过 {} 秒没有回应 —— 网络通不通?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;
        // 一版三栏：名字（认人）、多大、什么时候 —— 界面直接用，不必再自己算。
        Ok(json!({ "versions": versions }))
    }

    /// 云端有哪几款游戏 —— **不限于本机有的**。
    ///
    /// 这是"两台机器互相看得见"的入口：从前只有已知 id 才能列版本，第二台机器于是
    /// 不知道云端有什么。界面上的「刷新云端清单」按的就是它。
    ///
    /// 与「测试连接」同样的理由给一个总上限：这是一个按钮，用户盯着它等；而 rclone
    /// 那条路是"列目录 + 每个目录再列一次"，云端的游戏越多，调用越多。
    pub(in crate::daemon) async fn rpc_sync_cloud_games(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let games = tokio::time::timeout(CHECK_TIMEOUT, runner.cloud_games())
            .await
            .map_err(|_| {
                format!(
                    "列云端游戏超过 {} 秒没有回应 —— 网络通不通?云端的东西是不是太多了?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;
        Ok(json!({ "games": games }))
    }

    /// Put a game's saves back. Without `version`, the newest state wins.
    pub(in crate::daemon) async fn rpc_sync_restore(
        &self,
        game_id: &str,
        version: Option<&str>,
    ) -> Result<Value, String> {
        // A bad snapshot name is invalid input, not a sync failure: reject it
        // here, before anything runs, and as a JSON-RPC error.
        if let Some(version) = version
            && !sync::is_snapshot(version)
        {
            return Err(format!(
                "不是合法的版本名: {version}（形如 20260911T101500Z）"
            ));
        }

        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let (name, targets) = self.sync_targets(game_id).await?;
        let cloud_id = self.cloud_id_of(game_id).await?;

        let cloud_key = self.cloud_key_of(game_id).await?;
        let outcome = tokio::time::timeout(
            RESTORE_TIMEOUT,
            runner.restore(
                game_id,
                &name,
                &cloud_key,
                &targets,
                cloud_id.as_deref(),
                version,
            ),
        )
        .await
        .map_err(|_| {
            format!(
                "恢复《{name}》超过 {} 分钟没有结束 —— 网络通不通?",
                RESTORE_TIMEOUT.as_secs() / 60
            )
        })?;
        self.sync.remember(game_id, "恢复", &outcome);
        Ok(json!({ "ok": outcome.ok, "game": outcome }))
    }

    /// Pull the newest cloud state before a game starts.
    ///
    /// Returns `None` when there is nothing to do (sync off, no save paths, no
    /// rclone). Any failure is reported in the returned object and **never**
    /// stops the launch: the user asked to play a game.
    pub(in crate::daemon) async fn sync_pull_before_launch(&self, game_id: &str) -> Option<Value> {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return None;
            }
            let game = config.games.get(game_id)?;
            if game.save_paths.is_empty() {
                return None;
            }
            // 这一款的开关关着 ⇒ **不自动取回**。手动「取回存档」是用户自己按的，
            // 走的是另一条路（`rpc_sync_restore`），不受这个开关限制。
            if !game.sync_enabled {
                tracing::debug!("{game_id}: 这一款的云同步开关关着，启动前不取回");
                return None;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };

        // ⚠ 取回那条路**绝不认领身份**：认领是上传的事。没认领过就是"还没配对"，
        // 闸门据此拒绝取回（宁可不动，也不猜）——见 `crate::sync::cloud`。
        let cloud_id = self.cloud_id_of(game_id).await.unwrap_or(None);
        // 这条路的失败**绝不能拦住启动**（用户要的是玩游戏），所以错误也变成回话。
        let cloud_key = match self.cloud_key_of(game_id).await {
            Ok(key) => key,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };

        let outcome = match tokio::time::timeout(
            PULL_TIMEOUT,
            runner.pull(game_id, &name, &cloud_key, &targets, cloud_id.as_deref()),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::warn!("{game_id}: 启动前拉取超时（{PULL_TIMEOUT:?}），直接启动游戏");
                return Some(json!({
                    "ok": false,
                    "error": format!("拉取超过 {} 秒，已跳过", PULL_TIMEOUT.as_secs()),
                }));
            }
        };

        self.sync.remember(game_id, "取回", &outcome);
        if !outcome.ok {
            tracing::warn!("{game_id}: 启动前拉取失败: {:?}", outcome.error);
        }
        Some(serde_json::to_value(&outcome).unwrap_or(Value::Null))
    }

    /// Upload after a game exits.
    ///
    /// Runs detached from the session watcher: an upload must never hold up the
    /// engine, and the daemon may be asked to shut down while it runs.
    pub(in crate::daemon) async fn sync_after_game_exit(&self, game_id: &str) {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return;
            }
            // 这一款的开关关着 ⇒ **不自动上传**（手动「立即同步」不受限制）。
            if !config
                .games
                .get(game_id)
                .is_some_and(|game| game.sync_enabled)
            {
                tracing::debug!("{game_id}: 这一款的云同步开关关着，退出后不上传");
                return;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::debug!("{game_id}: 跳过退出后上传: {error}");
                return;
            }
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => {
                tracing::warn!("{game_id}: 退出后上传失败: {error}");
                return;
            }
        };

        // Let wineserver finish flushing whatever the game just wrote.
        tokio::time::sleep(SETTLE_DELAY).await;

        // 身份先定下来：包要带着它上云（第一次上传就在这一刻认领）。
        let packed = match self.pack_identity(&runner, game_id, &name, &targets).await {
            Ok(packed) => packed,
            Err(error) => {
                tracing::warn!("{game_id}: 退出后上传失败: {error}");
                return;
            }
        };
        let outcome = runner
            .upload(
                game_id,
                &name,
                &packed.cloud_key,
                &targets,
                Some(&packed.identity),
            )
            .await;
        self.sync.remember(game_id, "上传", &outcome);
        if outcome.ok {
            // 索引：**尽力而为** —— 这里出错只记日志，绝不让同步报错。
            self.refresh_index_for(&runner, game_id).await;
            tracing::info!("{game_id}: 退出后已同步存档");
        } else {
            tracing::warn!("{game_id}: 退出后同步失败: {:?}", outcome.error);
        }
    }
}

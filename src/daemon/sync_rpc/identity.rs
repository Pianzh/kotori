//! 云同步的「身份」：本机这一款 = 云端哪一条档案。
//!
//! 这一层只做两件事：**认领**（第一次上传时给这一款定一个 `cloud_id`，之后粘住）
//! 与**报出本机的机器身份**（`machine_id`）。
//!
//! 为什么身份不能是游戏名、也不能是配置里的那个 id（`games.<id>`）：两台机器可能
//! 给同一款游戏起不同的名字（于是永远对不上），也可能**同一个名字是两款不同的
//! 游戏**（slug 归一化，于是把别人的存档铺进来 —— 静默损坏存档那条路）。
//!
//! 粘住是刻意的：身份一旦会自己变，跨机器就再也说不清"我刚才传的那一版是谁的"。
//! 指纹只当**提议**，改身份要用户点头（配对界面）。
//!
//! 与 `mod.rs` 分开：那边是"设置与状态"，这边只碰身份本身。

use serde_json::{Value, json};
use uuid::Uuid;

use super::Daemon;
use crate::sync::SaveTarget;
use crate::sync::cloud::{MachineIdentity, PackIdentity};
use crate::sync::runner::Runner;

/// 上传这一刻定下来的东西：包清单里那份身份，以及**这一款在云端的落点**。
///
/// 落点必须一起交出去：rclone 那边"包放哪个目录"由身份决定（两台机器的游戏名不一样
/// 时，靠它把版本放进同一处），所以紧接着的这一次上传就得用这个键。
pub(super) struct Packed {
    pub identity: PackIdentity,
    pub cloud_key: String,
}

impl Daemon {
    /// 后台把存量档案缺的 exe 指纹补上。
    ///
    /// 这是**本机自己的事**，用户不需要看见它：指纹没算出来只意味着"这一款暂时认不出
    /// 云端那一款"，界面上没有任何东西可点。所以跟着启动跑一次（幂等、只补缺的），
    /// 一块盘不在就算那一款这次补不上，下次启动再说。
    pub(in crate::daemon) fn spawn_fingerprint_backfill(&self) {
        let this = self.clone_shares();
        tokio::spawn(async move {
            let missing = this
                .config
                .read()
                .await
                .games
                .values()
                .filter(|game| game.exe_fingerprint.is_none())
                .count();
            if missing == 0 {
                return;
            }
            let filled = this
                .mutate_config(|config| Ok(json!(crate::sync::fingerprint::fill_missing(config))))
                .await;
            match filled {
                Ok(filled) => {
                    tracing::info!("补齐了 {filled} 款游戏的 exe 指纹（还差 {missing} 款）")
                }
                Err(error) => tracing::warn!("补齐 exe 指纹失败: {error}"),
            }
        });
    }

    /// 这一款在云端的落点（本机 id 是缺省值：还没上传过的游戏就用它）。
    ///
    /// 传给引擎的**所有**调用都走这个键，别用游戏 id —— 那个只是本机的名字。
    pub(super) async fn cloud_key_of(&self, game_id: &str) -> Result<String, String> {
        let config = self.config.read().await;
        let game = config
            .games
            .get(game_id)
            .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
        Ok(game
            .cloud_dir
            .clone()
            .unwrap_or_else(|| game_id.to_string()))
    }

    /// 这一款在云端的身份；还没认领过就是 `None`。
    pub(super) async fn cloud_id_of(&self, game_id: &str) -> Result<Option<String>, String> {
        let config = self.config.read().await;
        let game = config
            .games
            .get(game_id)
            .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
        Ok(game.cloud_id.clone())
    }

    /// 上传这一版要写进包里的身份。
    ///
    /// 顺序是有讲究的：**指纹先算**（没有指纹就没法在云端认出同一款），然后才是
    /// "这一款在云端是谁"（本地认领过就用，否则按指纹找，找不到就新建），最后把
    /// 自己这台机器的信息并进身份卡写回云端。
    pub(super) async fn pack_identity(
        &self,
        runner: &Runner,
        game_id: &str,
        name: &str,
        targets: &[SaveTarget],
    ) -> Result<Packed, String> {
        let fingerprint = self.ensure_fingerprint(game_id).await?;
        let local = self.cloud_id_of(game_id).await?;
        let local_key = self.cloud_key_of(game_id).await?;
        let machine_id = self.machine_id().await?;
        let locations: Vec<String> = targets.iter().map(|target| target.key.clone()).collect();

        let resolved = runner
            .resolve_identity(
                game_id,
                name,
                local.as_deref(),
                MachineIdentity {
                    machine_id: machine_id.clone(),
                    label: machine_label(),
                    // 没有指纹就空着：**绝不编一个**（那会让两台机器认错人）。
                    fingerprints: fingerprint.clone().into_iter().collect(),
                    // 这台机器上这一款**配了**哪些位置 —— 位置对齐（§2.7）要的就是这份
                    // 清单，而不是"这一次恰好有文件的那几个"。
                    locations: locations.clone(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;

        // 新认领（或者按指纹认出了云端那一条）的身份、以及它的落点都要落盘：从这
        // 以后**粘住**。两者都没变就不动配置文件（每次上传写一次 config 是看得见
        // 的副作用）。
        let identity_changed = local.as_deref() != Some(resolved.identity.cloud_id.as_str());
        let key_changed = local_key != resolved.key;
        if identity_changed {
            tracing::info!(
                "{game_id}: {}云端身份 {}（落点 {key}）",
                if resolved.known { "认领" } else { "新建" },
                crate::sync::cloud::short_id(&resolved.identity.cloud_id, 8),
                key = resolved.key
            );
        }
        if identity_changed || key_changed {
            self.remember_identity(game_id, &resolved.identity.cloud_id, &resolved.key)
                .await?;
        }

        Ok(Packed {
            cloud_key: resolved.key,
            identity: PackIdentity {
                cloud_id: resolved.identity.cloud_id,
                machine_id: Some(machine_id),
                fingerprint,
                locations,
            },
        })
    }

    /// 这一款 exe 的指纹；缺了就当场算一次并落盘（读不到就如实返回 `None`）。
    ///
    /// 这是"存量补齐"的单条版本：同步页开一次会把整个库补齐（`sync.fingerprints`），
    /// 而上传这条路自己也得保证手上有指纹，否则第一次上传就认不出云端已有的那一款。
    async fn ensure_fingerprint(&self, game_id: &str) -> Result<Option<String>, String> {
        let (known, exe) = {
            let config = self.config.read().await;
            let game = config
                .games
                .get(game_id)
                .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
            (game.exe_fingerprint.clone(), game.exe_path.clone())
        };
        if let Some(fingerprint) = known {
            return Ok(Some(fingerprint));
        }
        let Some(fingerprint) = crate::sync::fingerprint::of_file(&exe) else {
            tracing::warn!(
                "{game_id}: 算不出 exe 指纹（{}），这一版不带指纹",
                exe.display()
            );
            return Ok(None);
        };

        let owner = game_id.to_string();
        let value = fingerprint.clone();
        self.mutate_config(move |config| {
            if let Some(game) = config.games.get_mut(&owner) {
                game.exe_fingerprint = Some(value);
            }
            Ok(Value::Null)
        })
        .await?;
        Ok(Some(fingerprint))
    }

    /// 把认领下来的身份与它的云端落点写进配置（落盘、粘住）。
    pub(super) async fn remember_identity(
        &self,
        game_id: &str,
        cloud_id: &str,
        cloud_key: &str,
    ) -> Result<(), String> {
        let owner = game_id.to_string();
        let cloud_id = cloud_id.to_string();
        let cloud_key = cloud_key.to_string();
        self.mutate_config(move |config| {
            let game = config
                .games
                .get_mut(&owner)
                .ok_or_else(|| format!("配置中找不到游戏: {owner}"))?;
            game.cloud_id = Some(cloud_id);
            game.cloud_dir = Some(cloud_key);
            Ok(Value::Null)
        })
        .await
        .map(|_| ())
    }

    /// 本机的机器身份：第一次要它时生成并落盘。
    ///
    /// 用 `get_or_insert` 而不是"先读再写"，是因为两个上传可能同时走到这里 ——
    /// 谁都行，但**一台机器只能有一个**（身份卡上要拿它对账）。
    pub(super) async fn machine_id(&self) -> Result<String, String> {
        if let Some(id) = self.config.read().await.daemon.machine_id.clone() {
            return Ok(id);
        }
        let minted = Uuid::new_v4().to_string();
        let candidate = minted.clone();
        let value = self
            .mutate_config(move |config| {
                let id = config.daemon.machine_id.get_or_insert(candidate);
                Ok(json!(id.clone()))
            })
            .await?;
        Ok(value.as_str().map(str::to_string).unwrap_or(minted))
    }
}

/// 这台机器叫什么，只给人看。
///
/// **绝不参与任何判断**：主机名会变（改配置、重装、同一台机器双系统），也可能两台
/// 机器重名。认机器靠 `machine_id`（一次性 uuid），这里纯粹是让身份卡读起来像人话。
fn machine_label() -> String {
    #[cfg(unix)]
    {
        nix::unistd::gethostname()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

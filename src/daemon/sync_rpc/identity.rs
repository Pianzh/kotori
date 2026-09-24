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
    /// 把存量档案里缺的 exe 指纹补上，返回这一次真的补了几条。
    ///
    /// 指纹**只在用得着的时候算**，不在启动时扫全库：添加游戏时算（`game.create`）、
    /// exe 换了时重算（`game.update`）、以及这里 —— 配对扫描前把缺的一次补齐。启动扫
    /// 全库的代价是每次开机都要去碰每一个 exe，而一块盘不在就白等一次 IO，补上了也
    /// 没人看。
    ///
    /// 幂等：已经有指纹的一条都不碰（见 `sync::fingerprint::fill_missing`）。
    pub(in crate::daemon) async fn fill_fingerprints(&self) -> Result<usize, String> {
        let missing = self
            .config
            .read()
            .await
            .games
            .values()
            .filter(|game| game.exe_fingerprint.is_none())
            .count();
        if missing == 0 {
            return Ok(0);
        }
        let filled = self
            .mutate_config(|config| Ok(json!(crate::sync::fingerprint::fill_missing(config).len())))
            .await?;
        let filled = filled.as_u64().unwrap_or(0) as usize;
        if filled > 0 {
            tracing::info!(
                "补上了 {filled} 款游戏的 exe 指纹（还有 {} 款读不到 exe）",
                missing - filled
            );
        }
        Ok(filled)
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

    /// "我这台机器 + 这一款"的那条身份：指纹、机器名、配了哪些存档位置、用过哪些 exe。
    ///
    /// 上传（写进身份卡）与索引（写进索引里那一条）共用它 —— 两处必须说同一句话，
    /// 否则"卡里有这个指纹、索引里没有"这种事迟早发生。
    ///
    /// 指纹**按需算**（没有就当场算一次并落盘）：没有指纹就没法在云端认出同一款。
    pub(super) async fn machine_identity_of(
        &self,
        game_id: &str,
    ) -> Result<MachineIdentity, String> {
        let fingerprint = self.ensure_fingerprint(game_id).await?;
        let (locations, parents, exe_path) = {
            let config = self.config.read().await;
            let game = config
                .games
                .get(game_id)
                .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
            // 这台机器上这一款**配了**哪些位置 —— 位置对齐（§2.7）要的就是这份清单，
            // 而不是"这一次恰好有文件的那几个"。
            let locations = game
                .save_paths
                .iter()
                .map(crate::sync::save_key)
                .collect::<Vec<String>>();
            // 弱匹配那一栏（用户 2026-09-24）：位置的**父目录名**，由原始路径算 ——
            // 整条 `save_key` 跨机器常常对不上（末段目录名各写各的）。
            let parents = game
                .save_paths
                .iter()
                .filter_map(|save| crate::sync::parent_dir(&save.path))
                .collect::<Vec<String>>();
            (
                locations,
                parents,
                game.exe_path.to_string_lossy().to_string(),
            )
        };
        Ok(MachineIdentity {
            machine_id: self.machine_id().await?,
            label: machine_label(),
            // 没有指纹就空着：**绝不编一个**（那会让两台机器认错人）。
            fingerprints: fingerprint.into_iter().collect(),
            locations,
            parents,
            // 用过的 exe 路径：只是参考信息（见 `MachineIdentity::exe_paths`）。
            exe_paths: (!exe_path.is_empty())
                .then_some(exe_path)
                .into_iter()
                .collect(),
        })
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
        _targets: &[SaveTarget],
    ) -> Result<Packed, String> {
        let local = self.cloud_id_of(game_id).await?;
        let local_key = self.cloud_key_of(game_id).await?;
        let machine = self.machine_identity_of(game_id).await?;

        let resolved = runner
            .resolve_identity(game_id, name, local.as_deref(), machine.clone())
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
                machine_id: Some(machine.machine_id),
                // 包里只带**一个**指纹（这一台机器此刻用的那个）；卡里的那一串是历史。
                fingerprint: machine.fingerprints.first().cloned(),
                locations: machine.locations,
            },
        })
    }

    /// 这一款 exe 的指纹；缺了就当场算一次并落盘（读不到就如实返回 `None`）。
    ///
    /// 这是"按需补齐"的单条版本：配对扫描前会把整个库补齐（`fill_fingerprints`），
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

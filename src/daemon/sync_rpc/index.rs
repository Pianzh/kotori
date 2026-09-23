//! 云端索引：一个桶一份的"云端现在有什么"（设计与护栏见 `crate::sync::index`）。
//!
//! 这个模块只做四件事：
//!
//!   1. **上传之后**把动过的那一条并进索引（[`Daemon::refresh_index_for`]）；
//!   2. **深度扫描之后**整份重建（[`Daemon::rebuild_index`]，配对扫描顺带做，因为那时
//!      所有身份卡都在手上）；
//!   3. 读出来给「云端存档」页（[`Daemon::rpc_sync_cloud_list`]）；
//!   4. 与本机配置对照：哪一条已配对、哪一条本机没有。
//!
//! ⚠ **写入一律"尽力而为"**：索引是加速用的镜像，不是真相（真相是身份卡）。写失败只记
//! 日志 —— **绝不许**让上传失败，包才是要紧的东西。

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{CHECK_TIMEOUT, Daemon, Runner};
use crate::sync::cloud::GameIdentity;
use crate::sync::index::{CloudIndex, IndexGame};

impl Daemon {
    /// 这一款现在该记成索引里的哪一条。
    ///
    /// 只用**本机配置 + 云端版本名列表**，**不读身份卡**：别的机器记下的指纹/位置由
    /// [`Runner::update_index`] 合并时保留，不必我们再去读一遍。
    async fn index_entry(
        &self,
        runner: &Runner,
        game_id: &str,
        cloud_key: &str,
    ) -> Result<IndexGame, String> {
        let (cloud_id, name) = {
            let config = self.config.read().await;
            let game = config
                .games
                .get(game_id)
                .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
            (
                game.cloud_id
                    .clone()
                    .ok_or_else(|| format!("{game_id} 还没有云端身份"))?,
                game.name.clone(),
            )
        };

        let versions = runner
            .packages(cloud_key)
            .await
            .map_err(|e| e.to_string())?;
        let latest = versions.last().cloned();

        let mut identity = GameIdentity::new(&cloud_id, &name);
        identity.merge_machine(self.machine_identity_of(game_id).await?);
        let mut game = IndexGame::from_identity(cloud_key, identity);
        // 大小那一刀（§12 D2）再填；现在如实当"不知道"。
        game.versions = versions.len();
        game.latest = latest;
        Ok(game)
    }

    /// 上传成功之后把这一条并进索引。**尽力而为**：写不进去只记日志。
    ///
    /// 调用点在"这一版已经上云了"之后 —— 索引没记上顶多让「云端存档」页晚一步看见它，
    /// 而让上传报错会让人以为存档没保住。
    pub(super) async fn refresh_index_for(&self, runner: &Runner, game_id: &str) {
        let cloud_key = match self.cloud_key_of(game_id).await {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上（拿不到云端落点）: {error}");
                return;
            }
        };
        let entry = match self.index_entry(runner, game_id, &cloud_key).await {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上: {error}");
                return;
            }
        };
        let machine_id = match self.machine_id().await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上（拿不到机器 id）: {error}");
                return;
            }
        };
        match runner.update_index(&machine_id, vec![entry]).await {
            Ok(index) => tracing::debug!("{game_id}: 索引已更新（云端 {} 款）", index.len()),
            Err(error) => tracing::warn!("{game_id}: 索引没记上: {error}"),
        }
    }

    /// 深度扫描之后整份重建索引：身份卡是真相，索引照它重写一份。
    ///
    /// 顺带把每款的版本数问出来（rclone 是每款列一次目录、kopia 一次列全部）——
    /// 这条路本来就是"慢路"，多这一下不改变什么。
    pub(super) async fn rebuild_index(&self, runner: &Runner, cards: &[(String, GameIdentity)]) {
        let counts: HashMap<String, usize> = match runner.cloud_games().await {
            Ok(games) => games
                .into_iter()
                .map(|game| (game.id, game.versions))
                .collect(),
            Err(error) => {
                tracing::warn!("索引重建: 列版本数失败（先记 0）: {error}");
                HashMap::new()
            }
        };
        let changes: Vec<IndexGame> = cards
            .iter()
            .map(|(key, identity)| {
                let mut game = IndexGame::from_identity(key, identity.clone());
                game.versions = counts.get(key).copied().unwrap_or(0);
                game
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let machine_id = match self.machine_id().await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!("索引重建失败（拿不到机器 id）: {error}");
                return;
            }
        };
        match runner.update_index(&machine_id, changes).await {
            Ok(index) => tracing::info!("云端索引已重建：{} 款", index.len()),
            Err(error) => tracing::warn!("索引重建失败: {error}"),
        }
    }

    /// `sync.cloud_list`：云端现在有哪些游戏（读**索引**，不读身份卡）。
    ///
    /// 回包里每一行都带上"本机哪一条与它配对" —— 那是本地信息（比对 `cloud_id`），
    /// 一次网络都不用打。`indexed: false` 表示桶里还没建过索引（老桶、或索引被删了），
    /// 此时 `games` 是空的，界面该提示去点「深度扫描云端」。
    pub(in crate::daemon) async fn rpc_sync_cloud_list(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let index = tokio::time::timeout(CHECK_TIMEOUT, runner.read_index())
            .await
            .map_err(|_| {
                format!(
                    "读云端索引超过 {} 秒没有回应 —— 网络通不通?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;

        // 本机这一侧：哪个身份被哪一条档案认了（本地查表，不碰网络）。
        let (paired, rejected) = {
            let config = self.config.read().await;
            let mut paired: HashMap<String, (String, String)> = HashMap::new();
            let mut rejected: Vec<String> = Vec::new();
            for (id, game) in &config.games {
                if let Some(cloud_id) = &game.cloud_id {
                    paired.insert(cloud_id.clone(), (id.clone(), game.name.clone()));
                }
                rejected.extend(game.cloud_rejected.iter().cloned());
            }
            (paired, rejected)
        };

        let indexed = index.is_some();
        let games: Vec<Value> = index
            .unwrap_or_else(CloudIndex::new)
            .games
            .into_iter()
            .map(|game| {
                let cloud_id = game.identity.cloud_id.clone();
                let local = paired.get(&cloud_id);
                // 本机明确否过这一条（配对表那笔账）：界面上要能说"你之前说了不是它"。
                let rejected_before = rejected.contains(&cloud_id);
                json!({
                    "cloud_key": game.cloud_key,
                    "cloud_id": cloud_id,
                    "name": game.identity.name,
                    "machines": game.identity.machines.len(),
                    "versions": game.versions,
                    "latest": game.latest,
                    "size": game.size,
                    // 用过的 exe 路径：只给人看、只给搜索用（不参与任何判断）。
                    "exe_paths": game
                        .identity
                        .machines
                        .iter()
                        .flat_map(|machine| machine.exe_paths.clone())
                        .collect::<Vec<String>>(),
                    "local_id": local.map(|(id, _)| id.clone()).unwrap_or_default(),
                    "local_name": local.map(|(_, name)| name.clone()).unwrap_or_default(),
                    "rejected": rejected_before,
                })
            })
            .collect();

        Ok(json!({ "indexed": indexed, "games": games }))
    }
}

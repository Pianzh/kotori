//! `sync.status`：设置页要的那一份状态（**没有一个密钥值**）。
//!
//! 从 `sync_rpc/mod.rs` 拆出来：那个文件是这一族的入口（常量、类型、runner 的构造、设置
//! 与凭据的写入），而"报一份状态"这一段自己就有一百三十多行，还牵着云端索引（每款当前绑
//! 的是哪一条、那一版多大 —— 都从本机缓存那份索引里查，零网络）。

use super::*;

impl Daemon {
    /// Everything the settings page needs, with no secret values in it.
    pub(in crate::daemon) async fn rpc_sync_status(&self) -> Result<Value, String> {
        // ⚠ **拿快照，不是拿读锁**：下面还要去读云端索引（`cloud_index_view`），而它自己
        // 也要再取一次配置读锁。把读锁跨着那个 `await` 持有会**自己等自己** —— tokio 的
        // 读写锁是公平的：一旦有写者（用户点「给这一款新建一条」时的 `sync.resolve`）在
        // 排队，这次新的读请求也得排在那个写者后面，而写者正等着我们手里的读锁。整台
        // daemon 就此僵住（2026-09-26 用户报的"点新建之后卡在启动中"）。
        let config = self.config.read().await.clone();
        let settings = config.sync.clone();
        let rclone =
            sync::find_rclone(&settings.rclone_binary).map(|p| p.to_string_lossy().to_string());
        let kopia =
            sync::find_kopia(&settings.kopia_binary).map(|p| p.to_string_lossy().to_string());
        // 当前生效的引擎要哪个二进制。选中的那个没装 = 没准备好；另一个没有
        // 不影响什么（用户可以只装一个）。
        let engine_binary = match settings.engine {
            SyncEngine::Rclone => rclone.clone(),
            SyncEngine::Kopia => kopia.clone(),
        };
        let secrets: Vec<&str> = self
            .sync
            .keyring()
            .present()
            .into_iter()
            .map(|key| key.account())
            .collect();

        // Only meaningful once sync is on; an unfinished setup is not an error
        // while the user is still typing.
        // 只有"开着同步"时才谈"为什么跑不起来"：还在填的过程中不算错。
        // 判据本身在 `actions::readiness_problem`（它只依赖参数，所以能单独测）。
        let problem = if settings.enabled {
            actions::readiness_problem(&settings, &self.sync.keyring(), engine_binary.as_deref())
        } else {
            None
        };

        let records = self
            .sync
            .records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default();

        // 每款还要报"它现在绑的是云端哪一条"（用户 2026-09-24：单游戏页要显示当前绑定，
        // 并且能换绑 / 新建）。云端名字与摘要从**本机缓存那份索引**里查（零网络，见
        // `cloud_index_view`）；查不到就空着 —— 那是"索引里还没有这一条"，不是错误。
        let index = match self.cloud_index_view(false).await {
            Ok(view) => view.index,
            Err(error) => {
                tracing::debug!("报同步状态时读不到索引（不影响别的）: {error}");
                None
            }
        };
        let cloud_of = |cloud_id: &str| -> (String, u64, String, u64) {
            let Some(index) = &index else {
                return (String::new(), 0, String::new(), 0);
            };
            match index
                .games
                .iter()
                .find(|game| game.identity.cloud_id == cloud_id)
            {
                Some(game) => (
                    game.identity.name.clone(),
                    game.versions as u64,
                    game.latest.clone().unwrap_or_default(),
                    game.size,
                ),
                None => (String::new(), 0, String::new(), 0),
            }
        };

        let mut games: Vec<Value> = config
            .games
            .iter()
            .map(|(id, game)| {
                // Report a location that cannot be resolved *now* (an unplugged
                // disk, a removed prefix) instead of failing later at sync time.
                let (count, location_problem) = match sync::targets(game, &config) {
                    Ok(targets) => (targets.len(), Value::Null),
                    Err(error) => (0, Value::String(error)),
                };
                let cloud_id = game.cloud_id.clone().unwrap_or_default();
                let (cloud_name, cloud_versions, cloud_latest, cloud_size) = if cloud_id.is_empty()
                {
                    (String::new(), 0, String::new(), 0)
                } else {
                    cloud_of(&cloud_id)
                };
                json!({
                    "id": id,
                    "name": game.name,
                    "locations": count,
                    "location_problem": location_problem,
                    "last": records.get(id),
                    // 当前绑定：身份 id 与落点是本机记的，名字与摘要是索引里的镜像。
                    "cloud_id": cloud_id,
                    "cloud_key": game.cloud_dir.clone().unwrap_or_default(),
                    "cloud_name": cloud_name,
                    "cloud_versions": cloud_versions,
                    "cloud_latest": cloud_latest,
                    "cloud_size": cloud_size,
                })
            })
            .collect();
        games.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });

        Ok(json!({
            "settings": settings,
            "enabled": settings.enabled,
            "engine": settings.engine,
            "engine_label": settings.engine.label(),
            "rclone": rclone,
            "kopia": kopia,
            "keyring": {
                "backend": self.sync.keyring().describe(),
                "ephemeral": self.sync.keyring().is_ephemeral(),
                // Which store, and whether it still needs a password. The UI
                // needs both to offer "unlock" instead of "enter credentials".
                "store": self.sync.keyring().kind(),
                "secrets_file": self.sync.secrets_path().display().to_string(),
                "min_master_password": crate::secrets::encrypted::MIN_MASTER_PASSWORD,
            },
            // Which entries exist — never what they contain.
            "secrets": secrets,
            "ready": settings.enabled && problem.is_none() && engine_binary.is_some(),
            "problem": problem,
            // rclone 那条路的远端；kopia 整个仓库落在 `kopia_prefix` 下。
            "remote": sync::remote_root(&settings),
            "kopia_prefix": sync::engine::repo_prefix(&settings),
            "pull_timeout_secs": PULL_TIMEOUT.as_secs(),
            "keep_versions_max": MAX_KEEP_VERSIONS,
            "games": games,
        }))
    }
}

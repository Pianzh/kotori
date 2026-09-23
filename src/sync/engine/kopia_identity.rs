//! kopia 那边的身份卡：拍成一条只装着 `kotori-game.json` 的快照。
//!
//! 与 `kopia.rs`（一版存档怎么拍）分开：身份不是存档，两者的读法也不一样 ——
//! 存档靠版本名筛，身份靠 `kind=identity` 标签筛，而**读一次身份 = 起一个 kopia
//! 进程**（`restore`），所以上层要缓存（§5.4）。
//!
//! 为什么不用一个普通对象存身份：kopia 的仓库是它自己的私有格式，没有"每款游戏一个
//! 目录"这回事，能放东西的地方只有快照。

use std::path::Path;

use super::super::SyncError;
use super::super::cloud::{self, GameIdentity};
use super::kopia::{COMMAND_TIMEOUT, Kopia};
use super::kopia_args as args;
use super::kopia_parse as parse;

impl Kopia {
    // ── 身份卡 ──────────────────────────────────────────────────────────────
    // kopia 的仓库是它自己的私有格式，没有"每款游戏一个目录"可以摆身份卡，所以
    // 身份也拍成一条快照：源目录里**只有**那份 `kotori-game.json`。读它要一次
    // `kopia restore`（§5.4：一次读 = 起一个进程），所以上层要缓存。

    /// 把一条身份快照恢复出来读成一张卡。
    async fn identity_from_snapshot(
        &self,
        snapshot_id: &str,
        work_dir: &Path,
    ) -> Result<Option<GameIdentity>, SyncError> {
        // 每次读都从一个干净的空目录开始：restore 是往目标里铺文件。
        let into = work_dir.join("identity-restore");
        let _ = std::fs::remove_dir_all(&into);
        std::fs::create_dir_all(&into)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", into.display())))?;
        self.run(
            &args::restore_args(snapshot_id, &into.to_string_lossy()),
            COMMAND_TIMEOUT,
        )
        .await?;

        let path = into.join(cloud::IDENTITY_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SyncError::Command(format!("身份快照里没有 {}: {e}", path.display())))?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| SyncError::Command(format!("云端身份卡读不懂: {e}")))
    }

    /// 云端与这个身份对应的卡（读-改-写里的"读"）。
    ///
    /// `game_id` 在这里用不上：kopia 那边身份就是 `game:` 标签的值本身。
    pub(super) async fn read_identity(
        &self,
        game_id: &str,
        cloud_id: &str,
        work_dir: &Path,
    ) -> Result<Option<GameIdentity>, SyncError> {
        let _ = game_id;
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_args(cloud_id), COMMAND_TIMEOUT)
            .await?;
        let snapshots = parse::identity_snapshots(&listed).map_err(SyncError::Command)?;
        let Some((_, snapshot_id)) = snapshots.into_iter().find(|(id, _)| id == cloud_id) else {
            return Ok(None);
        };
        self.identity_from_snapshot(&snapshot_id, work_dir).await
    }

    /// 云端**所有**身份卡（"按指纹找同一款"要用）。
    ///
    /// 一次列全部 + 每个身份一次 restore：慢，所以只在第一次上传一款游戏、或者配对
    /// 时才走这里（§5.4）。
    pub(super) async fn read_identities(
        &self,
        work_dir: &Path,
    ) -> Result<Vec<(String, GameIdentity)>, SyncError> {
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_all_args(), COMMAND_TIMEOUT)
            .await?;
        let wanted = parse::identity_snapshots(&listed).map_err(SyncError::Command)?;

        let mut identities = Vec::new();
        for (cloud_id, snapshot_id) in wanted {
            if let Some(identity) = self.identity_from_snapshot(&snapshot_id, work_dir).await? {
                // kopia 那边"键"就是身份本身（`game:` 标签的值），没有目录这回事。
                identities.push((cloud_id, identity));
            }
        }
        Ok(identities)
    }

    /// 把身份卡放回云端：写进一个只有它的目录，再对这个目录拍一条身份快照。
    pub(super) async fn write_identity(
        &self,
        game_id: &str,
        identity: &GameIdentity,
        work_dir: &Path,
    ) -> Result<String, SyncError> {
        let _ = game_id;
        let source = work_dir.join("identity").join(&identity.cloud_id);
        std::fs::create_dir_all(&source)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", source.display())))?;
        let text = serde_json::to_string_pretty(identity)
            .map_err(|e| SyncError::Command(format!("身份卡序列化失败: {e}")))?;
        std::fs::write(source.join(cloud::IDENTITY_FILE), text)
            .map_err(|e| SyncError::Command(format!("写不了身份卡: {e}")))?;

        self.ensure_connected().await?;
        self.run(
            &args::identity_snapshot_args(&identity.cloud_id, &source.to_string_lossy()),
            COMMAND_TIMEOUT,
        )
        .await?;
        // kopia 那边"这一款在云端的键"就是身份本身。
        Ok(identity.cloud_id.clone())
    }
}

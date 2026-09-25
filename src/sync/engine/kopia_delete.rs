//! kopia 那边的删除：删一版 / 删这一款的所有版 / 删掉身份快照（"词条"）。
//!
//! 从 `kopia.rs` 拆出来：那边是"拍快照 / 拉快照 / 列快照"，而这三个是**破坏性**的。
//!
//! ⚠ 存档快照与身份快照是**两族**：存档靠版本名（`description`）认，身份靠 `kind=identity`
//! 标签认 —— 所以删词条要的是 `cloud_id`（身份），不是落点。

use super::super::SyncError;
use super::kopia::{COMMAND_TIMEOUT, Kopia};
use super::kopia_args as args;
use super::kopia_parse as parse;

impl Kopia {
    pub(super) async fn remove(&self, cloud_key: &str, stamp: &str) -> Result<(), SyncError> {
        let snapshots = self.snapshots(cloud_key).await?;
        let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.description == stamp)
        else {
            // 已经不在了：保留窗口是 best-effort，不需要为此报错。
            // ⚠ 用户显式点的那次删除**不能**走这条静默路 —— 上层会先确认这一版真的在
            //（`sync.delete_version` 先列一次），到这儿就一定是有的。
            return Ok(());
        };
        self.run(&args::snapshot_delete_args(&snapshot.id), COMMAND_TIMEOUT)
            .await
            .map(|_| ())
    }

    /// 删掉这一款在云端的**所有**存档快照（身份快照不动）。
    ///
    /// 身份快照是另一族（靠 `kind=identity` 标签认），`snapshots()` 只看存档那族，
    /// 所以这里删不到卡上。
    pub(super) async fn remove_all(&self, cloud_key: &str) -> Result<usize, SyncError> {
        let mut removed = 0;
        for snapshot in self.snapshots(cloud_key).await? {
            self.run(&args::snapshot_delete_args(&snapshot.id), COMMAND_TIMEOUT)
                .await?;
            removed += 1;
        }
        Ok(removed)
    }

    /// 删掉这一款的**身份快照**（"词条"）。
    ///
    /// ⚠ 这里要的是 `cloud_id`（身份），不是落点：kopia 的身份快照就是按
    /// `game:<cloud_id>` + `kind=identity` 认的，没有"目录"这回事。
    pub(super) async fn remove_identity(&self, cloud_id: &str) -> Result<(), SyncError> {
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_args(cloud_id), COMMAND_TIMEOUT)
            .await?;
        let wanted = parse::identity_snapshots(&listed).map_err(SyncError::Command)?;
        for (id, snapshot_id) in wanted {
            if id == cloud_id {
                self.run(&args::snapshot_delete_args(&snapshot_id), COMMAND_TIMEOUT)
                    .await?;
            }
        }
        Ok(())
    }
}

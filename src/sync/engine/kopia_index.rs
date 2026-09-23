//! kopia 那边的索引：一条只装着 `kotori-index.json` 的快照。
//!
//! 与 `kopia_identity.rs` 分开：身份是"哪一款是哪一款"，索引是"云端现在有什么"。
//! 两者的读法不同（索引每次同步都要读一次，身份只在认领时读），混在一起会看不清
//! 谁贵谁便宜。
//!
//! **一个桶一份**：合并快照占 [`INDEX_MAIN`] 这个槽位，每次写入附带的那条增量占自己
//! 名字的槽位。槽位互不覆盖，所以两台机器同时同步也不会互相抹掉（见 `crate::sync::index`）。

use std::path::Path;

use super::super::SyncError;
use super::super::index::{CloudIndex, INDEX_FILE, INDEX_MAIN, IndexBundle};
use super::kopia::{COMMAND_TIMEOUT, Kopia};
use super::kopia_args as args;
use super::kopia_index_args as index_args;

impl Kopia {
    /// 把一条索引快照恢复出来读成索引。
    async fn index_from_snapshot(
        &self,
        snapshot_id: &str,
        work_dir: &Path,
        slot: &str,
    ) -> Result<Option<CloudIndex>, SyncError> {
        // 每个槽位一个落脚点：`restore` 是往目标目录里铺文件，共用会互相踩。
        let into = work_dir.join("index-restore").join(sanitize(slot));
        let _ = std::fs::remove_dir_all(&into);
        std::fs::create_dir_all(&into)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", into.display())))?;
        self.run(
            &args::restore_args(snapshot_id, &into.to_string_lossy()),
            COMMAND_TIMEOUT,
        )
        .await?;

        let path = into.join(INDEX_FILE);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SyncError::Command(format!("索引快照里没有 {}: {e}", path.display())))?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| SyncError::Command(format!("云端索引读不懂: {e}")))
    }

    /// 读云端索引：合并快照 + **还没并进去**的那些增量。
    ///
    /// 一次 `snapshot list`（便宜）+ 每个需要的槽位一次 `restore`（贵，所以只读需要的）。
    pub(super) async fn read_index(&self, work_dir: &Path) -> Result<IndexBundle, SyncError> {
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_all_args(), COMMAND_TIMEOUT)
            .await?;
        let slots = index_args::index_snapshots(&listed).map_err(SyncError::Command)?;

        let main = match slots.iter().find(|(slot, _)| slot == INDEX_MAIN) {
            Some((_, id)) => self.index_from_snapshot(id, work_dir, INDEX_MAIN).await?,
            // 桶里还没有索引：这不是错误，是"第一次"（上层据此提示深扫一次）。
            None => None,
        };

        let mut deltas = Vec::new();
        for (slot, id) in slots {
            if slot == INDEX_MAIN {
                continue;
            }
            if let Some(main) = &main
                && !main.needs(&slot)
            {
                continue;
            }
            // 与 rclone 那条路同一句话：一条坏增量不拖垮整份索引（真相在身份卡）。
            match self.index_from_snapshot(&id, work_dir, &slot).await {
                Ok(Some(index)) => deltas.push((slot, index)),
                Ok(None) => {}
                Err(error) => tracing::warn!("索引增量 {slot} 读不了，先跳过: {error}"),
            }
        }
        Ok(IndexBundle { main, deltas })
    }

    /// 写一条增量（**先写它**：槽位唯一，谁也覆盖不了谁）。
    pub(super) async fn write_index_delta(
        &self,
        name: &str,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        self.write_index_snapshot(name, index, work_dir).await
    }

    /// 重写合并快照（**后写它**）。
    pub(super) async fn write_index_main(
        &self,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        self.write_index_snapshot(INDEX_MAIN, index, work_dir).await
    }

    /// 把一个索引拍成一条快照。
    async fn write_index_snapshot(
        &self,
        slot: &str,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        let source = work_dir.join("index").join(sanitize(slot));
        std::fs::create_dir_all(&source)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", source.display())))?;
        let text = serde_json::to_string_pretty(index)
            .map_err(|e| SyncError::Command(format!("索引序列化失败: {e}")))?;
        std::fs::write(source.join(INDEX_FILE), text)
            .map_err(|e| SyncError::Command(format!("写不了索引: {e}")))?;

        self.ensure_connected().await?;
        self.run(
            &index_args::index_snapshot_args(slot, &source.to_string_lossy()),
            COMMAND_TIMEOUT,
        )
        .await?;
        Ok(())
    }
}

/// 槽位名会当目录名用，收成安全的字符（槽位本身就是机器 id + 时间戳，本来就是安全的）。
fn sanitize(slot: &str) -> String {
    slot.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

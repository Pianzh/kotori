//! rclone 那边的索引读写：桶里的 `index/` 那一层。
//!
//! 从 `rclone.rs` 拆出来（那边是"一版存档怎么上来下去"）：索引是**另一类对象**，而且
//! 它有一套自己的读法（列一次目录 + 只读没并过的那几条增量，见 `crate::sync::index`）。
//!
//! 桶里的布局：`<root>/index/kotori-index.json` 是并集快照，
//! `<root>/index/log/<machine>-<时间戳>.json` 是每次写入附带的一条增量。

use std::path::Path;

use crate::sync::index::{CloudIndex, INDEX_FILE, IndexBundle};
use crate::sync::{
    SyncError, cat_args, copyto_args, index_delta_path, index_log_path, index_main_path,
    list_files_args,
};

use super::rclone::{COMMAND_TIMEOUT, RcloneZip};

impl RcloneZip {
    // 桶里的布局：`<root>/index/kotori-index.json` 是并集快照，
    // `<root>/index/log/<machine>-<时间戳>.json` 是每次写入附带的一条增量。

    /// 读一个小 json 对象；对象不在就是 `None`（"还没有"与"读坏了"分得开）。
    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        remote: &str,
        what: &str,
    ) -> Result<Option<T>, SyncError> {
        let listed = match remote.rsplit_once('/') {
            Some((dir, name)) => match self.run(&list_files_args(dir), COMMAND_TIMEOUT).await {
                Ok(listed) => listed.lines().any(|line| line.trim() == name),
                Err(_) => false,
            },
            None => false,
        };
        if !listed {
            return Ok(None);
        }
        let text = self.run(&cat_args(remote), COMMAND_TIMEOUT).await?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| SyncError::Command(format!("云端{what}读不懂: {e}")))
    }

    /// 读云端索引：合并快照 + **还没并进去**的那些增量。
    pub(super) async fn read_index(&self) -> Result<IndexBundle, SyncError> {
        // 桶里还没有 `index/` 时 `lsf` 会失败：先 mkdir（幂等，也顺便把桶建出来）。
        self.run(
            &["mkdir".to_string(), index_log_path(&self.settings)],
            COMMAND_TIMEOUT,
        )
        .await?;

        // ⚠ 一份**读不懂的合并快照**当"没有"处理，而不是硬报错：索引只是镜像（真相在
        // 身份卡那边），它坏了必须还能靠「深度扫描云端」重写一份出来 —— 硬报错的话
        // `update_index` 一上来就读它，连重建都做不了，人就卡死在那儿了。
        let main: Option<CloudIndex> = match self
            .read_json(&index_main_path(&self.settings), "索引")
            .await
        {
            Ok(main) => main,
            Err(error) => {
                tracing::warn!("云端索引读不懂，先当没有（深度扫描会重写一份）: {error}");
                None
            }
        };
        let listed = self
            .run(
                &list_files_args(&index_log_path(&self.settings)),
                COMMAND_TIMEOUT,
            )
            .await?;

        let mut deltas = Vec::new();
        for line in listed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            // 桶是用户的：名字不像我们自己写的增量就不碰（与版本包同一条规矩）。
            if !crate::sync::index::is_delta_name(line) {
                continue;
            }
            // 已经并进合并快照的增量不必再下载一遍 —— 这正是"读 K 次"能成立的原因。
            if let Some(main) = &main
                && !main.needs(line)
            {
                continue;
            }
            // ⚠ 一条读坏了的增量只警告、不拖垮整份索引：索引是镜像，真相在身份卡那边，
            // 而"看云端有什么"这件事不该被桶里一个坏对象挡住（下次深扫会重写它）。
            match self
                .read_json::<CloudIndex>(&index_delta_path(&self.settings, line), "索引")
                .await
            {
                Ok(Some(index)) => deltas.push((line.to_string(), index)),
                Ok(None) => {}
                Err(error) => tracing::warn!("索引增量 {line} 读不了，先跳过: {error}"),
            }
        }
        Ok(IndexBundle { main, deltas })
    }

    /// 写一条增量（**先写它**：对象名唯一，谁也覆盖不了谁，所以并发同步不会丢条目）。
    pub(super) async fn write_index_delta(
        &self,
        name: &str,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        let remote = index_delta_path(&self.settings, name);
        self.push_json(&remote, index, work_dir, "index-delta")
            .await
    }

    /// 重写合并快照（**后写它**：它把已经并过的增量名字记在 `merged` 里）。
    pub(super) async fn write_index_main(
        &self,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        let remote = index_main_path(&self.settings);
        self.push_json(&remote, index, work_dir, "index-main").await
    }

    /// 把一个 json 推到桶里：先落在 `work_dir`（rclone 只搬文件），再 `copyto`。
    async fn push_json<T: serde::Serialize>(
        &self,
        remote: &str,
        value: &T,
        work_dir: &Path,
        name: &str,
    ) -> Result<(), SyncError> {
        std::fs::create_dir_all(work_dir)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", work_dir.display())))?;
        let local = work_dir.join(format!("{name}-{INDEX_FILE}"));
        let text = serde_json::to_string_pretty(value)
            .map_err(|e| SyncError::Command(format!("索引序列化失败: {e}")))?;
        std::fs::write(&local, text)
            .map_err(|e| SyncError::Command(format!("写不了 {}: {e}", local.display())))?;
        if let Some((dir, _)) = remote.rsplit_once('/') {
            self.run(&["mkdir".to_string(), dir.to_string()], COMMAND_TIMEOUT)
                .await?;
        }
        self.run(
            &copyto_args(&local.to_string_lossy(), remote),
            COMMAND_TIMEOUT,
        )
        .await?;
        Ok(())
    }
}

//! 临时工作区：下载一个包、解开它、把该覆盖的文件铺回存档目录。
//!
//! 单独成文件，是因为它是"临时文件"这件事的唯一落点：目录在数据目录下建，
//! `Drop` 时自己删掉（成功失败都删）。**不放在存档目录旁边**——那儿多出来的
//! 临时文件会被下一次打包收进包里。

use std::path::{Path, PathBuf};

use super::super::archive::{self, MergePlan};
use crate::sync::SaveTarget;

/// One operation's scratch directory. Removes itself on drop.
pub(super) struct Staging {
    path: PathBuf,
}

impl Staging {
    /// Create a fresh scratch directory under `root`.
    pub(super) fn new(root: &Path) -> Result<Self, String> {
        let path = root.join(format!("stage-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("无法创建临时目录 {}: {e}", path.display()))?;
        Ok(Self { path })
    }

    /// Where the engine builds this version's artifact (a zip, or a directory
    /// tree for kopia). The engine decides the shape; this only hands it a
    /// fresh empty directory that goes away with the `Staging`.
    pub(super) fn root(&self) -> &Path {
        &self.path
    }

    /// Where the package is unpacked to.
    pub(super) fn unpacked(&self) -> PathBuf {
        self.path.join("unpacked")
    }

    /// Lay the files the plan says to take down over the local save directory.
    ///
    /// Returns how many files were written. Only `plan.take` is touched: with
    /// [`archive::Merge::Newer`] that is exactly the set the cloud has a newer
    /// copy of, so a local save the user just made cannot be overwritten here.
    pub(super) fn lay_down(
        &self,
        targets: &[SaveTarget],
        plan: &MergePlan,
    ) -> Result<usize, String> {
        let root = self.unpacked();
        let mut written = 0;
        for entry in &plan.take {
            // 包里有、这台机器没配置的位置：`plan` 已经把这类条目挡在外面了，
            // 这里再查一次只是因为目标目录得从 `targets` 里找。
            let Some(target) = targets.iter().find(|target| target.key == entry.key) else {
                continue;
            };
            let source = root.join(entry.name());
            let destination = target
                .local
                .join(entry.path.split('/').collect::<PathBuf>());
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("无法创建 {}: {e}", parent.display()))?;
            }
            std::fs::copy(&source, &destination)
                .map_err(|e| format!("无法写入 {}: {e}", destination.display()))?;
            // `fs::copy` 不保证带上修改时间，而"谁新"全靠它——把清单里的值盖回去。
            archive::apply_mtime(&destination, entry.mtime_ms)?;
            written += 1;
        }
        Ok(written)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // 失败也要删：留在磁盘上的半成品包除了占地方没有别的用。
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

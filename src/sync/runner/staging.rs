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

/// 清掉**上一次**留下的残骸，返回删掉几个。
///
/// [`Staging`] 靠 `Drop` 删自己，而 `Drop` 在**进程被杀、崩溃、断电**时不会跑 ——
/// `stage-*` 于是会一直躺在工作目录里。Linux 上 `/tmp` 有 systemd-tmpfiles 之类帮忙
/// 收，但这些目录在**数据目录**下，没有谁管；Windows 上 `%TEMP%` 本身也不像 Linux
/// 那样自动清理。一个包小的几 MB、大的上百 MB，攒着就是白占磁盘。
///
/// **只在 daemon 启动时调用**，这一点是安全的：daemon 有单实例锁（见
/// [`crate::daemon::ipc`]），拿到锁就意味着没有别的实例在跑；而同步只发生在 daemon
/// 里（CLI 的 `kotori sync` 也是发 RPC 过来的）。删不掉的（Windows 上句柄还没放开、
/// 权限不对）直接跳过，不纠缠。
pub(crate) fn sweep_stale(work_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(work_dir) else {
        // 目录还不存在 = 从来没同步过，没什么可清的。
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        // 只碰我们自己造的那种名字：这个目录里将来可能还有别的东西。
        if !entry.file_name().to_string_lossy().starts_with("stage-") {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用完就删的临时目录（单测惯例，同 `executables.rs` 里那个）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-staging-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 上次崩溃留下的 `stage-*` 要被清掉，而**不是我们造的东西一个都不许碰**。
    #[test]
    fn stale_stage_directories_are_swept_and_others_are_left_alone() {
        let dir = TempDir::new("sweep");
        std::fs::create_dir_all(dir.0.join("stage-11111111")).unwrap();
        std::fs::create_dir_all(dir.0.join("stage-22222222")).unwrap();
        // 将来这个目录里可能放别的东西（比如某个引擎自己的缓存）：那不是我们的。
        std::fs::create_dir_all(dir.0.join("kopia-cache")).unwrap();

        assert_eq!(sweep_stale(&dir.0), 2);
        assert!(!dir.0.join("stage-11111111").exists());
        assert!(!dir.0.join("stage-22222222").exists());
        assert!(dir.0.join("kopia-cache").exists(), "不是我们造的不许碰");
    }

    /// 从来没同步过的机器：目录都不存在，不该被当成错误。
    #[test]
    fn sweeping_a_directory_that_was_never_created_is_not_an_error() {
        let missing = std::env::temp_dir().join(format!(
            "kotori-staging-missing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        assert_eq!(sweep_stale(&missing), 0);
    }
}

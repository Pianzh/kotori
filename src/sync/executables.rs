//! 两个引擎的可执行文件在哪。
//!
//! 从前只有两步（`KOTORI_*` 环境变量 → `PATH`）。2026-09-16 用户要的是"在设置页
//! 直接指路"：Windows 上"把 kopia 放在一个目录里、PATH 里什么都不加"才是常态，
//! 而 PATH 这件事普通人根本不会配。所以现在是四步，每一步都有理由：
//!
//! 1. **设置页里填的那一个** —— 用户明确指定的，最优先（他填了就是他说了算）；
//! 2. `KOTORI_RCLONE` / `KOTORI_KOPIA` —— 项目约定的注入点（ADR-006）：集成测试
//!    指向假货靠它，高级用户临时换一个也靠它；
//! 3. **kotori 自己的目录** —— 随包内置时二进制就躺在它旁边。这是 Windows 上
//!    "开箱即用"那条路的落点；Linux 上 `/usr/bin` 也刚好命中，行为与从前一致；
//! 4. `PATH`。
//!
//! 前三步都要求**那里真的有个文件**：填错的路径不会静默退到 PATH，否则用户会以为
//! 自己填的生效了。[`misconfigured`] 就是给"填了但没有"留的那句实话。

use std::path::{Path, PathBuf};

/// 找 rclone。`configured` 是设置页里那一行 —— 目录、完整文件路径，或者空。
pub fn find_rclone(configured: &str) -> Option<PathBuf> {
    find(configured, "rclone", "KOTORI_RCLONE")
}

/// 找 kopia。
pub fn find_kopia(configured: &str) -> Option<PathBuf> {
    find(configured, "kopia", "KOTORI_KOPIA")
}

/// 设置了位置、但那里找不到这个程序 —— 给用户看的一句话；没设置或找得到就是 `None`。
///
/// 单独一个函数，是因为 [`find_rclone`] 只回答"用哪个"，说不清"你为什么没找到"：
/// 设置页那一栏填错时，用户需要知道的是**他填的那个位置不对**，而不是"PATH 里没有
/// rclone"——他家那个明明在 D 盘。
pub fn misconfigured(configured: &str, name: &str) -> Option<String> {
    let configured = configured.trim();
    if configured.is_empty() || locate(Path::new(configured), name).is_some() {
        return None;
    }
    Some(format!(
        "设置里那个 {name} 位置（{configured}）里找不到 {name}：填目录或完整文件路径都行，留空则由 kotori 自己去找"
    ))
}

fn find(configured: &str, name: &str, env_var: &str) -> Option<PathBuf> {
    if let Some(found) = locate(Path::new(configured.trim()), name) {
        return Some(found);
    }
    if let Some(explicit) = std::env::var_os(env_var)
        && let Some(found) = locate(Path::new(&explicit), name)
    {
        return Some(found);
    }
    if let Some(found) = beside_kotori(name) {
        return Some(found);
    }
    crate::util::executor::find_binary(name)
}

/// 用户给的那一行 → 可执行文件。
///
/// **目录和文件都收**：设置页那一栏问的是"程序在哪"，而 Windows 用户从资源管理器
/// 复制过来的十有八九是**目录**（用户原话就是"kopia 进程所在目录"），Linux 用户更
/// 可能给完整路径。两种都认一下，比逼用户去分辨省事。
///
/// ⚠ 给的是**文件**时**不检查文件名**：用户明确指了谁就是谁。Windows 上解压出来
/// 常叫 `rclone-v1.65.2-windows-amd64.exe`，按名字卡他等于让这一项没法用。
/// 给的是**目录**时才去找 `name` / `name.exe` —— 那里没有就是没有，不许往上层猜。
fn locate(path: &Path, name: &str) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    // `.exe` 也认：Windows 上内置的那个叫 `kopia.exe`，而用户给的多半是目录。
    [name.to_string(), format!("{name}.exe")]
        .into_iter()
        .map(|file| path.join(file))
        .find(|candidate| candidate.is_file())
}

/// kotori 自己旁边有没有这个程序（随包内置的落点）。
fn beside_kotori(name: &str) -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [name.to_string(), format!("{name}.exe")]
        .into_iter()
        .map(|file| dir.join(file))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用完就删的临时目录（单测惯例，同 `game/mod.rs` 里那个）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-exec-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// 造一个假程序，返回它的完整路径。
        fn program(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 目录和完整路径都认 —— 用户从资源管理器复制过来的是目录。
    #[test]
    fn a_directory_or_a_file_both_point_at_the_program() {
        let dir = TempDir::new("locate");
        let program = dir.program("rclone");

        assert_eq!(locate(&dir.0, "rclone"), Some(program.clone()));
        assert_eq!(locate(&program, "rclone"), Some(program));
        // 目录里没有这个东西就是没有 —— 不许往上层目录猜。
        assert_eq!(locate(&dir.0, "kopia"), None);
    }

    /// Windows 上内置的那个叫 `kopia.exe`，而设置页里填的是目录。
    #[test]
    fn a_windows_style_name_is_found_too() {
        let dir = TempDir::new("exe");
        let program = dir.program("kopia.exe");
        assert_eq!(locate(&dir.0, "kopia"), Some(program));
    }

    /// 空的设置 = 没设置：不参与查找，也不该被当成"路径错了"。
    #[test]
    fn an_empty_setting_is_not_a_location() {
        assert_eq!(locate(Path::new(""), "rclone"), None);
        assert_eq!(misconfigured("", "rclone"), None);
        assert_eq!(misconfigured("   ", "rclone"), None);
    }

    /// 填了、但那里什么都没有 —— 这句话必须说出来，否则用户以为自己填的生效了。
    #[test]
    fn a_configured_path_with_nothing_in_it_says_so() {
        let dir = TempDir::new("missing");
        let asked = dir.0.to_str().unwrap().to_string();

        let note = misconfigured(&asked, "rclone").unwrap();
        assert!(note.contains("找不到 rclone"), "{note}");
        assert!(note.contains(&asked), "要说清是哪个位置：{note}");

        // 填对了就没话说。
        let program = dir.program("rclone");
        assert_eq!(misconfigured(program.to_str().unwrap(), "rclone"), None);

        // 填了一个**目录**、但里面没有这个程序：那才是"没找到"。
        let empty = TempDir::new("empty-dir");
        assert!(
            misconfigured(empty.0.to_str().unwrap(), "rclone").is_some(),
            "空目录里没有 rclone，该说话"
        );

        // 而填一个**文件**就是"它在这儿"，名字不必叫 rclone —— Windows 上解压出来
        // 常带版本号，用户明确指了谁就信他（见 `locate` 的说明）。
        let renamed = dir.program("rclone-v1.65.2.exe");
        assert_eq!(misconfigured(renamed.to_str().unwrap(), "rclone"), None);
    }
}

//! Shared utilities.

pub mod exec;
pub mod executor;

use std::path::Path;

/// 两个路径指的是不是**同一个文件**?
///
/// 这是「自动追踪认不认这个进程」那条判决唯一的凭据(用户 2026-09-20:任务管理器里
/// 那一栏就是完整路径,名称已经含在里面了,所以比一次就够)。两样东西都试:
///
/// 1. `canonicalize` —— 解掉符号链接与 `.`/`..`;Windows 上还会统一成盘符真实大小写
///    与长文件名,这正是"同一个文件两种写法"最常出现的两个来源;
/// 2. 解不开(文件已经不在了、没权限、路径形状在别的文件系统上)就退回**字面**比较,
///    那时 Windows 忽略大小写、并把 `\` 与 `/` 看成同一个分隔符 —— 大小写与分隔符
///    是 Windows 路径比较里唯一可以安全放过的两个差别。
pub fn same_file(left: &Path, right: &Path) -> bool {
    let resolve = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (left, right) = (resolve(left), resolve(right));
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        loose(&left) == loose(&right)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 字面比较用的那一份:小写 + `/` 统一(只有 Windows 用它)。
#[cfg(windows)]
fn loose(path: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(path.to_string_lossy().replace('\\', "/").to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_file_under_two_spellings_is_one_file() {
        let dir = std::env::temp_dir();
        let file = dir.join("kotori-same-file-probe");
        std::fs::write(&file, b"x").unwrap();

        // 同一个路径、以及"绕一圈"的写法(中间多一层 `.`),都算同一个。
        assert!(same_file(&file, &file));
        assert!(same_file(&dir.join("./kotori-same-file-probe"), &file));

        // 不同的文件当然不算 —— 这条是"两款都叫 Game.exe 的游戏"那个坑的守门人。
        let other = dir.join("kotori-same-file-probe-2");
        std::fs::write(&other, b"x").unwrap();
        assert!(!same_file(&file, &other));

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&other);
    }
}

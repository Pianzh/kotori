//! 远端路径与键名：把 bucket/prefix、游戏 id 和存档位置翻译成 rclone 认得的
//! `remote:path`，以及云端每个存档位置该叫什么目录名。
//!
//! 与 `snapshots` 分开：这里只回答"东西放在哪"，包的命名、识别与保留策略
//! 全在那边。

use super::REMOTE;
use crate::config::SyncConfig;

/// `kotori:<bucket>/<prefix>` — the bucket is part of the remote path, which
/// keeps the environment-configured remote minimal.
pub fn remote_root(settings: &SyncConfig) -> String {
    let bucket = settings.bucket.trim().trim_matches('/');
    let prefix = settings.prefix.trim().trim_matches('/');
    let path = match (bucket.is_empty(), prefix.is_empty()) {
        (true, _) => prefix.to_string(),
        (false, true) => bucket.to_string(),
        (false, false) => format!("{bucket}/{prefix}"),
    };
    if path.is_empty() {
        format!("{REMOTE}:")
    } else {
        format!("{REMOTE}:{path}")
    }
}

/// Remote directory holding one game's version packages.
///
/// 一版一个包，所以这个目录里**只有**包：`<stamp>.zip`。"最新的一版"就是名字
/// 最大的那个包，不额外维护指针文件——少一个会写坏的东西。
pub fn game_remote(settings: &SyncConfig, game_id: &str) -> String {
    format!("{}/games/{game_id}", remote_root(settings))
}

/// Remote path of one version package.
pub fn package_remote(settings: &SyncConfig, game_id: &str, stamp: &str) -> String {
    format!(
        "{}/{stamp}{}",
        game_remote(settings, game_id),
        super::PACKAGE_SUFFIX
    )
}

/// 索引在桶里的目录：`<root>/index`。
///
/// 与 `games/<id>/` 平级、互不干扰：`games/` 那一层是包与身份卡，`index/` 这一层只有
/// 索引（一个桶一份，见 `crate::sync::index`）。
pub fn index_root(settings: &SyncConfig) -> String {
    format!("{}/{}", remote_root(settings), super::index::INDEX_DIR)
}

/// 合并快照的完整远端路径。
pub fn index_main_path(settings: &SyncConfig) -> String {
    format!("{}/{}", index_root(settings), super::index::INDEX_FILE)
}

/// 增量放的目录。
pub fn index_log_path(settings: &SyncConfig) -> String {
    format!("{}/{}", index_root(settings), super::index::INDEX_LOG)
}

/// 一条增量的完整远端路径。
pub fn index_delta_path(settings: &SyncConfig, name: &str) -> String {
    format!("{}/{}", index_log_path(settings), name)
}

/// A stable, readable directory name for one save location.
///
/// Derived from the location description (not its index) so that reordering
/// the list in the UI cannot scramble what is already in the cloud.
pub fn save_key(save: &crate::config::SavePath) -> String {
    let mut key: String = save
        .path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while key.contains("__") {
        key = key.replace("__", "_");
    }
    let key = key.trim_matches('_').to_string();
    let key = if key.is_empty() {
        "save".to_string()
    } else {
        key
    };
    let kind = match save.kind {
        crate::config::SavePathKind::Windows => "win",
        crate::config::SavePathKind::Relative => "rel",
        crate::config::SavePathKind::Absolute => "abs",
    };
    // Keep it readable but bounded, and disambiguate kinds that could collide.
    let short: String = key.chars().take(48).collect();
    format!("{kind}-{short}")
}

/// 原始存档路径的**父目录名**：弱匹配用的那一栏（用户 2026-09-24 定）。
///
/// 取倒数第二段：`%APPDATA%\Game\save` → `game`。为什么不整条比：两台机器给同一款
/// 游戏建的末段目录名常常不一样（`save` / `savedata` / `SaveData`），而游戏或厂商
/// 那一层通常是一致的。⚠ 它**只用来列候选**，永不自动绑（见 [`crate::sync::matching`]）。
///
/// 不比的情况：只有一段的（`savedata`）、倒数第二段是令牌（`%APPDATA%`）或盘符（`C:`）
/// 的 —— 那都是公共目录，不是"这一款自己的目录"。
pub fn parent_dir(path: &str) -> Option<String> {
    let parts: Vec<&str> = path
        .split(['\\', '/'])
        .map(str::trim)
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let parent = parts.get(parts.len().checked_sub(2)?)?;
    // 令牌与盘符都不是目录名。
    if parent.starts_with('%') && parent.ends_with('%') {
        return None;
    }
    if parent.chars().count() == 2 && parent.ends_with(':') {
        return None;
    }
    Some(parent.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SavePath, SavePathKind};
    use crate::sync::testing::settings;

    #[test]
    fn remote_paths_are_built_from_the_prefix() {
        let config = settings();
        // The bucket is part of the remote path, so the environment-configured
        // remote stays minimal.
        assert_eq!(remote_root(&config), "kotori:kotori-saves/prefix");
        assert_eq!(
            game_remote(&config, "3days"),
            "kotori:kotori-saves/prefix/games/3days"
        );
        // 一版一个包：远端目录里只有 `<stamp>.zip`。
        assert_eq!(
            package_remote(&config, "3days", "20260915T120000Z-1a2b3c4d"),
            "kotori:kotori-saves/prefix/games/3days/20260915T120000Z-1a2b3c4d.zip"
        );

        // An empty prefix stays valid, and so does an unset bucket.
        let mut bare = config.clone();
        bare.prefix = String::new();
        assert_eq!(remote_root(&bare), "kotori:kotori-saves");
        let mut bucketless = config;
        bucketless.bucket = String::new();
        assert_eq!(remote_root(&bucketless), "kotori:prefix");
    }

    /// 父目录名：取倒数第二段；令牌、盘符、只有一段的都不比。
    #[test]
    fn a_parent_dir_is_the_second_to_last_segment() {
        assert_eq!(parent_dir("%APPDATA%\\Game\\save").as_deref(), Some("game"));
        assert_eq!(
            parent_dir(r"C:\Games\Hoshi\savedata").as_deref(),
            Some("hoshi")
        );
        assert_eq!(parent_dir("savedata/sub").as_deref(), Some("savedata"));
        assert_eq!(parent_dir("savedata"), None, "只有一段，不比");
        assert_eq!(parent_dir("%APPDATA%\\save"), None, "令牌不是目录名");
        assert_eq!(parent_dir("C:\\save"), None, "盘符不是目录名");
        assert_eq!(parent_dir(""), None);
    }

    #[test]
    fn save_keys_are_stable_and_readable() {
        let windows = SavePath::new(SavePathKind::Windows, "%APPDATA%\\Game\\save");
        let relative = SavePath::new(SavePathKind::Relative, "savedata");
        assert_eq!(save_key(&windows), "win-appdata_game_save");
        assert_eq!(save_key(&relative), "rel-savedata");

        // Two locations that differ only by kind must not collide.
        let absolute = SavePath::new(SavePathKind::Absolute, "savedata");
        assert_ne!(save_key(&relative), save_key(&absolute));

        // Long paths are bounded; empty paths still produce a name.
        let long = SavePath::new(SavePathKind::Relative, "a".repeat(200).as_str());
        assert!(save_key(&long).len() <= 52);
        assert_eq!(
            save_key(&SavePath::new(SavePathKind::Relative, "///")),
            "rel-save"
        );
    }
}

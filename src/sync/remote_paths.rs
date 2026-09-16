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

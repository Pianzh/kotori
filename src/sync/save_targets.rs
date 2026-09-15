//! 本地存档目标：把配置里可移植的存档位置描述（`%APPDATA%\Game\save`、
//! `savedata`、`/home/me/...`）解析成这台机器上的真实目录。
//!
//! 与 `remote_paths` 分开：那边算云端的路径，这边算本机的路径；一个存档位置
//! 的云端目录名（`save_key`）在这里被带进 [`SaveTarget`]。

use std::path::PathBuf;

use super::save_key;
use crate::config::{Config, GameConfig};

/// One configured save location, resolved to a real directory on this machine.
///
/// This is the bridge between the portable description stored in the config
/// (`%APPDATA%\Game\save`, `savedata`, `/home/me/...`) and something rclone can
/// be pointed at. The `key` is derived from the description, not the index, so
/// reordering the list in the UI cannot scramble what is already in the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveTarget {
    pub key: String,
    /// The configured location, for user-facing messages.
    pub configured: String,
    /// Where it lives here and now.
    pub local: PathBuf,
    /// Glob patterns rclone must skip.
    pub exclude: Vec<String>,
}

/// Resolve every save location of a game into a local directory.
///
/// Fails loudly rather than silently syncing the wrong thing: a location that
/// cannot be resolved, or that resolves to the filesystem root (which would
/// mean "upload the whole disk"), aborts the whole game.
pub fn targets(game: &GameConfig, config: &Config) -> Result<Vec<SaveTarget>, String> {
    let (root, _) = crate::wine::SaveRoot::for_platform(game, config);
    let game_dir = game.effective_game_dir();

    let mut targets = Vec::with_capacity(game.save_paths.len());
    for save in &game.save_paths {
        let local = crate::wine::resolve_save_path(&root, &game_dir, save)?;
        if local.as_os_str().is_empty() {
            return Err(format!("存档位置「{}」解析为空路径", save.path));
        }
        // `/` has no parent; nothing legitimate about syncing a whole disk.
        if local.parent().is_none() {
            return Err(format!(
                "存档位置「{}」解析到了文件系统根目录，拒绝同步",
                save.path
            ));
        }
        targets.push(SaveTarget {
            key: save_key(save),
            configured: save.path.clone(),
            local,
            exclude: save.exclude.clone(),
        });
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_locations_resolve_to_this_machines_directories() {
        use crate::config::{GameConfig, SavePath, ScaleProfile};

        let dir = std::env::temp_dir().join(format!(
            "kotori-sync-targets-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let prefix = dir.join("prefix");
        let user = prefix.join("drive_c/users/tester");
        std::fs::create_dir_all(&user).unwrap();

        let mut config = Config::default();
        config.wine.prefix = Some(prefix.clone());
        let game = GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            save_paths: vec![
                SavePath::inferred("savedata"),
                SavePath::inferred("%APPDATA%\\Demo\\save"),
            ],
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        };

        let resolved = targets(&game, &config).unwrap();
        assert_eq!(resolved.len(), 2);
        // The cloud key comes from the *description*, so it is the same on
        // Windows and on wine (ADR-008) — and cannot be scrambled by reordering.
        assert_eq!(resolved[0].key, "rel-savedata");
        assert_eq!(resolved[0].local, PathBuf::from("/games/demo/savedata"));
        assert_eq!(resolved[1].key, "win-appdata_demo_save");
        assert_eq!(
            resolved[1].local,
            user.join("AppData/Roaming/Demo/save"),
            "the token resolves inside the wine prefix"
        );

        // Without a prefix the token still resolves (to the default ~/.wine).
        let bare = targets(&game, &Config::default()).unwrap();
        assert!(bare[1].local.ends_with("AppData/Roaming/Demo/save"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn syncing_the_filesystem_root_is_refused() {
        use crate::config::{GameConfig, SavePath, SavePathKind, ScaleProfile};

        let game = GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            // A typo here would mean "upload the whole disk".
            save_paths: vec![SavePath::new(SavePathKind::Absolute, "/")],
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        };

        let error = targets(&game, &Config::default()).unwrap_err();
        assert!(error.contains("根目录"), "{error}");
    }
}

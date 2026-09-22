//! `#[cfg(test)]` 共享脚手架:FakePrefix / game()。两边测试模块都用。

use super::*;
use crate::config::{GameConfig, ScaleProfile};

/// Build a throw-away wine prefix with the given user directories.
pub(super) struct FakePrefix(PathBuf);

impl FakePrefix {
    pub(super) fn new(tag: &str, users: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-wine-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let users_dir = dir.join("drive_c").join("users");
        for user in users {
            std::fs::create_dir_all(users_dir.join(user)).unwrap();
        }
        Self(dir)
    }

    pub(super) fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for FakePrefix {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub(super) fn game(game_dir: &str, exe: &str) -> GameConfig {
    GameConfig {
        cloud_id: None,
        exe_fingerprint: None,
        cloud_dir: None,
        cloud_rejected: Vec::new(),
        name: "demo".into(),
        game_dir: PathBuf::from(game_dir),
        exe_path: PathBuf::from(exe),
        launch_args: Vec::new(),
        save_paths: Vec::new(),
        wine_prefix: None,
        auto_watch: false,
        direct_launch: false,
        process_name: None,
        scale_profile: ScaleProfile::default_for(),
        created_at: chrono::Utc::now(),
    }
}

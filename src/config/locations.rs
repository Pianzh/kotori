//! 持久位置与运行时路径之间的边界。探测不写配置，迁移由现有写锁负责落盘。

use std::path::{Path, PathBuf};

use super::{Config, GameConfig, SavePath, SavePathKind};
use crate::mount::MountTable;

impl Config {
    pub fn capture_mounts(&mut self) {
        let table = MountTable::read();
        for game in self.games.values_mut() {
            game.capture_mounts(&table);
        }
    }
}

impl GameConfig {
    pub fn capture_mounts(&mut self, table: &MountTable) {
        if self.game_dir_mount.is_none() {
            self.game_dir_mount = table.infer(&self.game_dir);
        }
        if self.exe_mount.is_none() {
            self.exe_mount = table.infer(&self.exe_path);
        }
        // 挂载引用是主存储；旧绝对路径不再作为失效时的备用目标。
        if self.game_dir_mount.is_some() {
            self.game_dir.clear();
        }
        if self.exe_mount.is_some() {
            self.exe_path.clear();
        }
        for save in &mut self.save_paths {
            if save.kind == SavePathKind::Absolute && save.mount.is_none() {
                save.mount = table.infer(&crate::wine::expand_home(&save.path));
            }
            // 保留 save.path 作为云端旧包的稳定键，挂载位置变化不能改变包内成员名。
        }
    }

    pub fn resolved_exe(&self) -> Result<PathBuf, String> {
        self.resolved_exe_with(&MountTable::read())
    }

    pub fn resolved_exe_with(&self, table: &MountTable) -> Result<PathBuf, String> {
        if let Some(reference) = &self.exe_mount {
            return table.resolve(reference);
        }
        Ok(self.exe_path.clone())
    }

    pub fn resolved_game_dir(&self) -> Result<PathBuf, String> {
        self.resolved_game_dir_with(&MountTable::read())
    }

    pub fn resolved_game_dir_with(&self, table: &MountTable) -> Result<PathBuf, String> {
        if let Some(reference) = &self.game_dir_mount {
            return table.resolve(reference);
        }
        if !self.game_dir.as_os_str().is_empty() {
            return Ok(self.game_dir.clone());
        }
        self.resolved_exe_with(table)?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "游戏目录和可执行文件位置均未配置".into())
    }

    /// UI 只改排除项/排序时仍保留盘引用及旧同步键；真正换路径才重新识别。
    pub fn reconcile_save_paths(&self, incoming: &mut [SavePath]) {
        let table = MountTable::read();
        for save in incoming {
            if save.kind != SavePathKind::Absolute || save.mount.is_some() {
                continue;
            }
            let matched = self.save_paths.iter().find(|old| {
                old.kind == save.kind
                    && (old.path == save.path
                        || old
                            .mount
                            .as_ref()
                            .and_then(|m| table.resolve(m).ok())
                            .is_some_and(|p| p == Path::new(&save.path)))
            });
            if let Some(old) = matched {
                save.mount = old.mount.clone();
                save.path = old.path.clone();
            } else if let Some(reference) = table.infer(&crate::wine::expand_home(&save.path)) {
                // 新挑的盘上位置直接落引用；不在盘上的照旧存绝对路径。
                save.mount = Some(reference);
            }
        }
    }

    pub fn location_view(&self) -> serde_json::Value {
        let table = MountTable::read();
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            let dir = self.resolved_game_dir_with(&table);
            let exe = self.resolved_exe_with(&table);
            let error = dir.as_ref().err().or(exe.as_ref().err()).cloned();
            map.insert(
                "game_dir".into(),
                serde_json::json!(dir.unwrap_or_default()),
            );
            map.insert(
                "exe_path".into(),
                serde_json::json!(exe.unwrap_or_default()),
            );
            map.insert("location_error".into(), serde_json::json!(error));
            if let Some(saves) = map.get_mut("save_paths").and_then(|v| v.as_array_mut()) {
                for (save, view) in self.save_paths.iter().zip(saves) {
                    if let Some(path) = save.mount.as_ref().and_then(|m| table.resolve(m).ok()) {
                        view["path"] = serde_json::json!(path);
                    }
                }
            }
        }
        value
    }
}

//! 测试夹具：一个把"云端"摆在磁盘上的假 rclone。
//!
//! 它不是"记录调用的桩"，而是一个**能跑通闭环**的假货：`copyto` 真的搬文件，
//! `lsf` 真的列目录，`deletefile` 真的删。于是上传 → 取回 → 恢复 → 保留这四条路
//! 可以在没有任何网络、没有任何密钥的情况下端到端验一遍——桶就是 `<dir>/bucket`。
//!
//! 本文件只在 `cfg(test)` 下编译，里面的条目因此放宽到 `pub(super)`。

use std::path::{Path, PathBuf};

use super::Runner;
use crate::config::{SyncConfig, SyncEngine};
use crate::secrets::{Keyring, SecretKey};
use crate::sync::SaveTarget;

mod native {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/native.rs"
    ));
}

/// A stand-in `rclone` backed by a directory.
pub(super) struct FakeRclone {
    pub(super) dir: PathBuf,
    pub(super) bin: PathBuf,
}

impl FakeRclone {
    pub(super) fn new(tag: &str) -> Self {
        let dir = native::scratch(&format!("rclone-{tag}"));
        std::fs::create_dir_all(dir.join("bucket")).unwrap();
        let bin = dir.join("rclone");

        // wrapper 只负责每个 Runner 的独立路径；命令语义与 E2E 共用原生 helper。
        let quote =
            |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
        let script = format!(
            "#!/bin/sh\nexec {} --rclone-root {} --rclone-log {} --rclone-fail {} \"$@\"\n",
            quote(native::executable()),
            quote(&dir.join("bucket/kotori")),
            quote(&dir.join("log")),
            quote(&dir.join("fail")),
        );
        crate::secrets::testing::write_executable(&bin, &script);

        Self { dir, bin }
    }

    pub(super) fn settings(&self, keep_versions: u32) -> SyncConfig {
        SyncConfig {
            // ⚠ 引擎**必须写死成 rclone**:这个夹具的整个意义就是"假 rclone",而
            // `SyncConfig::default()` 的引擎在 2026-09-18 改成了 kopia —— 跟着默认值
            // 走的话,这些测试会全部跑去走 kopia 那条路。
            engine: SyncEngine::Rclone,
            enabled: true,
            endpoint: String::new(),
            bucket: "bkt".to_string(),
            prefix: "prefix".to_string(),
            keep_versions,
            ..SyncConfig::default()
        }
    }

    pub(super) fn keyring(&self) -> Keyring {
        let keyring = Keyring::memory();
        keyring.set(SecretKey::B2KeyId, "keyid123").unwrap();
        keyring.set(SecretKey::B2AppKey, "appkey456").unwrap();
        keyring
    }

    pub(super) fn runner(&self, keep_versions: u32) -> Runner {
        Runner::with_binary(&self.bin, self.settings(keep_versions), self.keyring())
            // 临时包绝不能落进真实数据目录（测试不许碰用户的家目录）。
            .with_work_dir(self.dir.join("work"))
    }

    /// Where one remote path lives on disk.
    pub(super) fn bucket_path(&self, remote: &str) -> PathBuf {
        self.dir.join("bucket").join(remote.replace(':', "/"))
    }

    /// Pretend something is already in the cloud.
    pub(super) fn put(&self, remote: &str, body: &str) {
        let path = self.bucket_path(remote);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Copy a local zip into the cloud as one of a game's version packages.
    pub(super) fn put_package(&self, game_id: &str, stamp: &str, zip: &Path) {
        let destination = self.package_path(game_id, stamp);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(zip, destination).unwrap();
    }

    pub(super) fn package_path(&self, game_id: &str, stamp: &str) -> PathBuf {
        self.bucket_path(&format!(
            "kotori:bkt/prefix/games/{game_id}/{stamp}{}",
            crate::sync::PACKAGE_SUFFIX
        ))
    }

    /// The packages in the cloud for one game, oldest first.
    pub(super) fn package_names(&self, game_id: &str) -> Vec<String> {
        let dir = self.bucket_path(&format!("kotori:bkt/prefix/games/{game_id}"));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_suffix(crate::sync::PACKAGE_SUFFIX)
                    .map(str::to_string)
            })
            .collect();
        names.sort();
        names
    }

    pub(super) fn remote_exists(&self, remote: &str) -> bool {
        self.bucket_path(remote).exists()
    }

    pub(super) fn fail_on(&self, needle: &str) {
        std::fs::write(self.dir.join("fail"), needle).unwrap();
    }

    /// Every rclone invocation, in order.
    pub(super) fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("argv:"))
            .map(str::to_string)
            .collect()
    }

    pub(super) fn env_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }

    pub(super) fn calls_matching(&self, needle: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.contains(needle))
            .collect()
    }
}

impl Drop for FakeRclone {
    fn drop(&mut self) {
        native::cleanup(&self.dir);
    }
}

pub(super) fn target(dir: &std::path::Path, configured: &str, key: &str) -> SaveTarget {
    SaveTarget {
        key: key.to_string(),
        configured: configured.to_string(),
        local: dir.to_path_buf(),
        exclude: Vec::new(),
    }
}

//! 测试夹具：一个把"云端"摆在磁盘上的假 rclone。
//!
//! 它不是"记录调用的桩"，而是一个**能跑通闭环**的假货：`copyto` 真的搬文件，
//! `lsf` 真的列目录，`deletefile` 真的删。于是上传 → 取回 → 恢复 → 保留这四条路
//! 可以在没有任何网络、没有任何密钥的情况下端到端验一遍——桶就是 `<dir>/bucket`。
//!
//! 本文件只在 `cfg(test)` 下编译，里面的条目因此放宽到 `pub(super)`。

use std::path::{Path, PathBuf};

use super::Runner;
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};
use crate::sync::SaveTarget;

/// A stand-in `rclone` backed by a directory.
pub(super) struct FakeRclone {
    pub(super) dir: PathBuf,
    pub(super) bin: PathBuf,
}

impl FakeRclone {
    pub(super) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-rclone-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("bucket")).unwrap();
        let bin = dir.join("rclone");

        let script = format!(
            r#"#!/bin/sh
[ "$1" = "{warmup}" ] && exit 0
dir='{dir}'
bucket="$dir/bucket"
{{
  echo "argv:$*"
  env | grep '^RCLONE_CONFIG' | sed 's/^/env:/' | sort
}} >> "$dir/log"
if [ -f "$dir/fail" ] && printf '%s' "$*" | grep -qF "$(cat "$dir/fail")"; then
  echo "fake rclone: refusing $1" >&2
  exit 1
fi
# 远端名 `kotori:bkt/prefix/...` 就是 bucket 下的路径,和真 rclone 一样把 `:`
# 当分隔符。
resolve() {{ printf '%s' "$1" | tr ':' '/'; }}
case "$1" in
  mkdir)
    mkdir -p "$bucket/$(resolve "$2")"
    ;;
  copyto)
    case "$2" in
      kotori:*) src="$bucket/$(resolve "$2")"; dst="$3" ;;
      *) src="$2"; dst="$bucket/$(resolve "$3")" ;;
    esac
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
    ;;
  lsf)
    target="$bucket/$(resolve "$3")"
    [ -d "$target" ] && ls -1 "$target"
    ;;
  deletefile)
    rm -f "$bucket/$(resolve "$2")"
    ;;
esac
exit 0
"#,
            dir = dir.display(),
            warmup = crate::secrets::testing::WARMUP_FLAG
        );
        crate::secrets::testing::write_executable(&bin, &script);

        Self { dir, bin }
    }

    pub(super) fn settings(&self, keep_versions: u32) -> SyncConfig {
        SyncConfig {
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
        std::fs::remove_dir_all(&self.dir).ok();
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

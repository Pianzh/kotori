//! 测试夹具：一个记录调用的假 rclone。
//!
//! `upload` / `restore` / 传输底座三处的测试都要同一个假 rclone，放在这里共用；
//! 本文件只在 `cfg(test)` 下编译，里面的条目因此放宽到 `pub(super)`。

use std::path::PathBuf;

use super::Runner;
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};
use crate::sync::SaveTarget;

/// A stand-in `rclone` that records how it was called.
///
/// It never touches the network, so the tests pin the *contract* — which
/// arguments kotori builds, and with which credentials in the environment —
/// rather than rclone's own behaviour.
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
        std::fs::create_dir_all(dir.join("lsf")).unwrap();
        let bin = dir.join("rclone");

        let script = format!(
            r#"#!/bin/sh
[ "$1" = "{warmup}" ] && exit 0
dir='{dir}'
{{
  echo "argv:$*"
  env | grep '^RCLONE_CONFIG' | sed 's/^/env:/' | sort
}} >> "$dir/log"
if [ -f "$dir/fail" ] && printf '%s' "$*" | grep -qF "$(cat "$dir/fail")"; then
  echo "fake rclone: refusing $1" >&2
  exit 1
fi
if [ "$1" = "obscure" ]; then
  # `rclone obscure -` reads the password from the first line of stdin; echo it
  # back "obscured" so a test can prove it travelled that way and not on argv.
  IFS= read -r line || line=""
  printf 'obscured-%s\n' "$line"
fi
if [ "$1" = "lsf" ]; then
  key=$(printf '%s' "$3" | tr '/:' '__')
  [ -f "$dir/lsf/$key" ] && cat "$dir/lsf/$key"
fi
exit 0
"#,
            dir = dir.display(),
            warmup = crate::secrets::testing::WARMUP_FLAG
        );
        crate::secrets::testing::write_executable(&bin, &script);

        Self { dir, bin }
    }

    pub(super) fn settings(&self, encryption: bool, keep_versions: u32) -> SyncConfig {
        SyncConfig {
            enabled: true,
            endpoint: String::new(),
            bucket: "bkt".to_string(),
            prefix: "prefix".to_string(),
            encryption,
            keep_versions,
        }
    }

    pub(super) fn keyring(&self, encryption: bool) -> Keyring {
        let keyring = Keyring::memory();
        keyring.set(SecretKey::B2KeyId, "keyid123").unwrap();
        keyring.set(SecretKey::B2AppKey, "appkey456").unwrap();
        if encryption {
            keyring.set(SecretKey::SyncPassword, "hunter2").unwrap();
            keyring
                .set(SecretKey::SyncPasswordObscured, "obscured-blob")
                .unwrap();
        }
        keyring
    }

    pub(super) fn runner(&self, encryption: bool, keep_versions: u32) -> Runner {
        Runner::with_binary(
            &self.bin,
            self.settings(encryption, keep_versions),
            self.keyring(encryption),
        )
    }

    /// Teach the fake what `lsf` should print for a remote path.
    pub(super) fn set_listing(&self, remote: &str, entries: &[&str]) {
        let key: String = remote
            .chars()
            .map(|c| if c == '/' || c == ':' { '_' } else { c })
            .collect();
        let body: String = entries.iter().map(|e| format!("{e}/\n")).collect();
        std::fs::write(self.dir.join("lsf").join(key), body).unwrap();
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

pub(super) const CURRENT: &str = "kotori:bkt/prefix/games/demo/current";
pub(super) const VERSIONS: &str = "kotori:bkt/prefix/games/demo/versions";

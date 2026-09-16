//! Cloud save sync (Phase 2).
//!
//! Design decisions (see HANDOVER.md ADR-010 / ADR-012):
//!   * **rclone** is the transfer engine, not kopia: it moves bytes to B2, and
//!     without a crypt layer the saves live in the bucket as plain files that
//!     can be recovered with any S3 tool — no kopia, no kotori, not even rclone.
//!   * **One package per version** (`<game_id>/<stamp>.zip`, see [`archive`]):
//!     every version is a complete point-in-time image of every save location, so
//!     "go back one version" is "lay that package down", not "un-pick a diff".
//!   * Retention is a *sliding window of packages*, and it is **off by default**
//!     (`keep_versions = 0` keeps everything). Pruning only ever deletes old
//!     packages in the cloud; local saves are never touched.
//!   * The automatic pre-launch pull only ever takes files that are *newer* in
//!     the cloud ([`archive::Merge::Newer`]). This used to be `rclone --update`;
//!     with packages it is our own merge code, and that is the one invariant that
//!     must not break (ADR-012).
//!
//! This module builds and parses rclone invocations; it does not depend on
//! rclone being installed, which keeps it unit-testable.
//!
//! 文件分工：本文件只留模块级文档、常量、[`SyncError`] 与子模块转发；每个功能的
//! 完整生命周期各占一个文件——
//!   * `validate`：动手之前先判定"能不能开始"（设置与凭据）；
//!   * `remote_paths`：东西放在哪个远端目录、存档位置叫什么名字；
//!   * `snapshots`：版本包怎么命名、怎么识别、保留窗口怎么算；
//!   * `archive`：一个包怎么打、怎么解、哪些文件该覆盖（纯本地，可单测）；
//!   * `rclone_args`：一次 rclone 调用该带哪些参数；
//!   * `rclone_env`：凭据怎么交给子进程，以及 rclone 可执行文件在哪；
//!   * `save_targets`：配置里的存档位置落到这台机器的哪个目录。

/// Remote name synthesised through rclone's environment configuration.
pub const REMOTE: &str = "kotori";

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("云同步未启用")]
    NotEnabled,
    #[error("云同步配置不完整: {0}")]
    Config(String),
    #[error("{engine} 未安装或不可执行: {detail}")]
    EngineMissing {
        engine: &'static str,
        detail: String,
    },
    #[error("{0}")]
    Command(String),
}

pub mod archive;
pub mod engine;
pub mod executables;
pub mod runner;

/// Suffix of a version package in the bucket.
pub const PACKAGE_SUFFIX: &str = ".zip";

/// Path that makes rclone ignore every config file. Windows spells it `NUL`,
/// and getting this wrong would silently make rclone read the user's own
/// `rclone.conf` — on a dual-boot machine with different settings per OS.
pub const fn null_config_path() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

mod rclone_args;
mod rclone_env;
mod remote_paths;
mod save_targets;
mod snapshots;
#[cfg(test)]
mod testing;
mod validate;

// 对外只做转发：这些名字原来就定义在 `sync` 下，`crate::sync::X` 这个路径
// （daemon、UI、tests/ipc_e2e.rs 都在用）必须一字不变。
pub use executables::{find_kopia, find_rclone, misconfigured};
pub use rclone_args::{copyto_args, deletefile_args, list_files_args};
pub use rclone_env::rclone_env;
pub use remote_paths::{game_remote, package_remote, remote_root, save_key};
pub use save_targets::{SaveTarget, targets};
pub use snapshots::{is_snapshot, parse_packages, prune_plan, version_stamp};
pub use validate::{validate, validate_endpoint, validate_secrets};

//! Cloud save sync (Phase 2).
//!
//! Design decisions (see HANDOVER.md ADR-010):
//!   * **rclone** is the transfer engine, not kopia: encryption becomes an
//!     optional `crypt` layer instead of a hard requirement, and without it the
//!     saves live in the bucket as plain files that can be recovered with any
//!     S3 tool — no kopia, no kotori, not even rclone.
//!   * Retention is a *sliding window of version snapshots*, and it is
//!     **off by default** (`keep_versions = 0` keeps everything). Pruning only
//!     ever deletes old snapshots in the cloud; local saves are never touched.
//!   * Restores use `rclone copy`, never `rclone sync`, so an empty or broken
//!     backup can never delete a local save.
//!
//! This module builds and parses rclone invocations; it does not depend on
//! rclone being installed, which keeps it unit-testable.
//!
//! 文件分工：本文件只留模块级文档、常量、[`SyncError`] 与子模块转发；每个功能的
//! 完整生命周期各占一个文件——
//!   * `validate`：动手之前先判定"能不能开始"（设置与凭据）；
//!   * `remote_paths`：东西放在哪个远端目录、存档位置叫什么名字；
//!   * `snapshots`：版本快照怎么命名、怎么识别、保留窗口怎么算；
//!   * `rclone_args`：一次 rclone 调用该带哪些参数；
//!   * `rclone_env`：凭据怎么交给子进程，以及 rclone 可执行文件在哪；
//!   * `save_targets`：配置里的存档位置落到这台机器的哪个目录。

/// Remote name synthesised through rclone's environment configuration.
pub const REMOTE: &str = "kotori";
/// Remote name carrying the optional `crypt` layer. Kept free of characters
/// that would break the `RCLONE_CONFIG_<NAME>_<OPTION>` mapping.
pub const REMOTE_CRYPT: &str = "kotorienc";

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("云同步未启用")]
    NotEnabled,
    #[error("云同步配置不完整: {0}")]
    Config(String),
    #[error("rclone 未安装或不可执行: {0}")]
    RcloneMissing(String),
    #[error("rclone 执行失败: {0}")]
    Command(String),
}

pub mod runner;

/// Where version snapshots live, relative to the configured prefix.
pub const VERSIONS_DIR: &str = "versions";
/// Where the "current" copy of each save location lives.
pub const CURRENT_DIR: &str = "current";

/// The fixed second factor mixed into the crypt key.
pub const DEFAULT_PASSWORD2: &str = "kotori";

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
pub use rclone_args::{
    Merge, copy_args, list_dirs_args, obscure_args, purge_args, push_excludes, restore_args,
};
pub use rclone_env::{find_rclone, rclone_env};
// `remote_name` 只被 `remote_root` 在模块内部使用；它原来就在 `crate::sync` 下，
// 路径必须保留，所以照旧转发（binary crate 里没有别的引用，需放行这条 lint）。
#[allow(unused_imports)]
pub use remote_paths::remote_name;
pub use remote_paths::{game_remote, remote_root, save_key, versions_remote};
pub use save_targets::{SaveTarget, targets};
pub use snapshots::{is_snapshot, parse_dirs, prune_plan, version_stamp};
pub use validate::{validate, validate_endpoint, validate_secrets};

//! Driving rclone: the orchestration half of cloud sync.
//!
//! [`super`] builds argument lists and `archive` knows what is inside a package;
//! this module runs rclone, resolves what each save location means on *this*
//! machine, and reports per-location outcomes that the UI and CLI can show
//! verbatim.
//!
//! Three rules shape everything here (ADR-010 / ADR-012):
//!   * **Never destroy local data.** A package is uploaded whole, the automatic
//!     pre-launch pull only takes files that are newer in the cloud, and pruning
//!     only ever deletes packages in the cloud.
//!   * **Secrets never touch a disk or a command line.** They are read from the
//!     keyring and handed to the child through its environment.
//!   * **A failure says what failed.** Every location gets its own outcome, so
//!     "synced" is never reported for something that was skipped.
//!
//! 文件分工：本文件是 [`Runner`] 本身——超时预算、"跑一次 rclone"的传输底座，
//! 以及"云端有哪些包"这几个查询；`upload.rs` 管上传，`pull.rs` 管启动前取回，
//! `restore.rs` 管恢复与保留窗口，`staging.rs` 管临时目录与铺文件，
//! `outcome.rs` 是汇报类型，`diagnostics.rs` 把 rclone 的 stderr 翻成人话，
//! `testing.rs` 是测试用的假 rclone。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::{
    SyncError, copyto_args, deletefile_args, game_remote, list_files_args, package_remote,
    parse_packages, rclone_env, remote_root, validate, validate_secrets,
};
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};

use self::diagnostics::explain_failure;
pub use self::outcome::{GameOutcome, LocationOutcome};

mod diagnostics;
mod outcome;
mod pull;
mod restore;
mod staging;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
mod upload;

/// Ceiling for one rclone invocation.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the automatic pull before a launch. Past this the game starts
/// anyway: a slow network must never keep the user out of their game.
pub const PULL_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace period before uploading after a game exits.
///
/// The session ends when the game's processes are gone, but wine's background
/// services (wineserver) can still be flushing a save to disk. Reading a file
/// mid-write would upload a truncated save, and that truncated copy is what
/// would come back on the next launch, so we wait a moment first.
pub const SETTLE_DELAY: Duration = Duration::from_secs(3);

/// Runs rclone against one sync configuration.
pub struct Runner {
    rclone: PathBuf,
    settings: SyncConfig,
    keyring: Keyring,
    /// 打包与解包的落脚点。默认在数据目录下（`~/.local/share/kotori/sync`），
    /// **不放在存档目录旁边**：那儿多出来的临时文件会被下一次打包收进去。
    work_dir: PathBuf,
}

impl Runner {
    /// Build a runner if rclone is available, otherwise say what is missing.
    pub fn new(settings: SyncConfig, keyring: Keyring) -> Result<Self, SyncError> {
        let rclone = super::find_rclone().ok_or_else(|| {
            SyncError::RcloneMissing(
                "PATH 里找不到 rclone。Arch: sudo pacman -S rclone".to_string(),
            )
        })?;
        Ok(Self::with_binary(rclone, settings, keyring))
    }

    /// A runner pinned to one binary. Used by tests, and by users who keep
    /// rclone somewhere unusual (`KOTORI_RCLONE` is handled by `new`).
    pub fn with_binary(rclone: impl Into<PathBuf>, settings: SyncConfig, keyring: Keyring) -> Self {
        Self {
            rclone: rclone.into(),
            settings,
            keyring,
            work_dir: crate::config::data_dir().join("sync"),
        }
    }

    /// Point the temporary work area somewhere else.
    ///
    /// 只给测试用：一次测试运行绝不该往真实数据目录里写包（生产路径永远走
    /// 数据目录下的 `sync/`）。
    #[cfg(test)]
    pub fn with_work_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.work_dir = dir.into();
        self
    }

    /// Structural check plus "are the credentials actually there".
    pub fn ready(&self) -> Result<(), SyncError> {
        validate(&self.settings)?;
        validate_secrets(&self.settings, &self.keyring)
    }

    /// Credentials, as the child process should see them.
    fn env(&self) -> Result<Vec<(String, String)>, SyncError> {
        let read = |key: SecretKey| {
            self.keyring
                .get(key)
                .map_err(|e| SyncError::Command(e.to_string()))
        };
        let missing = |what: &str| SyncError::Config(format!("密钥环里没有{what}"));

        let key_id = read(SecretKey::B2KeyId)?.ok_or_else(|| missing("B2 key id"))?;
        let app_key = read(SecretKey::B2AppKey)?.ok_or_else(|| missing("B2 application key"))?;
        let obscured = if self.settings.encryption {
            Some(
                read(SecretKey::SyncPasswordObscured)?
                    .ok_or_else(|| missing("同步密码（rclone 形态）"))?,
            )
        } else {
            None
        };

        Ok(rclone_env(
            &self.settings,
            &key_id,
            &app_key,
            obscured.as_deref(),
        ))
    }

    /// Run rclone with the credentials in its environment.
    async fn run(&self, args: &[String], timeout: Duration) -> Result<String, SyncError> {
        let env = self.env()?;
        self.run_with(args, timeout, env).await
    }

    /// `run_with_stdin`, with nothing on stdin.
    async fn run_with(
        &self,
        args: &[String],
        timeout: Duration,
        env: Vec<(String, String)>,
    ) -> Result<String, SyncError> {
        self.run_with_stdin(args, timeout, env, None).await
    }

    /// The one place an rclone child is spawned.
    ///
    /// `stdin` exists for `rclone obscure -` alone: it reads the password as
    /// the first line of stdin, which is what keeps the password out of `ps`.
    async fn run_with_stdin(
        &self,
        args: &[String],
        timeout: Duration,
        env: Vec<(String, String)>,
        stdin: Option<&str>,
    ) -> Result<String, SyncError> {
        let mut command = tokio::process::Command::new(&self.rclone);
        command
            .args(args)
            .envs(env)
            // Even the "no credentials" path must ignore the user's config.
            .env("RCLONE_CONFIG", super::null_config_path())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A timed-out transfer must not keep running in the background.
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.rclone.display())))?;

        if let Some(line) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            // Hand over the line and close the pipe, so the child is not left
            // waiting for more. A write error is not the verdict: an rclone too
            // old to read stdin never touches the pipe, and the exit status and
            // stderr are what actually say what happened (the same shape as the
            // keyring's EPIPE — see `secrets`).
            let _ = pipe.write_all(line.as_bytes()).await;
            let _ = pipe.write_all(b"\n").await;
            drop(pipe);
        }

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                SyncError::Command(format!("rclone {} 超过 {:?} 未完成", args[0], timeout))
            })?
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.rclone.display())))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                format!("退出码 {:?}", output.status.code())
            } else {
                explain_failure(&stderr)
            };
            return Err(SyncError::Command(format!(
                "rclone {} 失败: {detail}",
                args[0]
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Turn a user password into the form rclone stores.
    ///
    /// Deliberately delegated to rclone: a re-implementation that differs by one
    /// byte derives a different key, which would lock the user out of their own
    /// backups (ADR-010). The password goes in on **stdin** (`rclone obscure -`),
    /// so no command line ever carries it; sync runs themselves read the
    /// obscured form from the keyring and likewise put nothing on a command line.
    pub async fn obscure(&self, password: &str) -> Result<String, SyncError> {
        let output = self
            .run_with_stdin(
                &super::obscure_args(),
                COMMAND_TIMEOUT,
                Vec::new(),
                Some(password),
            )
            .await?;
        let obscured = output.trim().to_string();
        if obscured.is_empty() {
            return Err(SyncError::Command(
                "rclone obscure 没有返回结果，请检查 rclone 版本".to_string(),
            ));
        }
        Ok(obscured)
    }

    /// Verify credentials, bucket and write access in one call.
    ///
    /// `mkdir` on the prefix is the cheapest operation that exercises all three
    /// and it is idempotent, so a successful "test connection" also means the
    /// first sync will not fail for a trivial reason.
    pub async fn check(&self) -> Result<String, SyncError> {
        self.ready()?;
        let root = remote_root(&self.settings);
        self.run(&["mkdir".to_string(), root.clone()], COMMAND_TIMEOUT)
            .await?;
        Ok(root)
    }

    /// The version packages the cloud holds for a game, oldest first.
    pub async fn packages(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let remote = game_remote(&self.settings, game_id);
        let output = self.run(&list_files_args(&remote), COMMAND_TIMEOUT).await?;
        // Only ever report names that look like our own packages.
        Ok(parse_packages(&output))
    }

    /// The newest package, or `None` when the cloud has never seen this game.
    ///
    /// "Newest" is simply the largest name: the stamp starts with second-
    /// precision UTC, so lexicographic order is chronological order and no
    /// pointer file has to be kept in sync.
    pub async fn latest_package(&self, game_id: &str) -> Result<Option<String>, SyncError> {
        Ok(self.packages(game_id).await?.pop())
    }

    /// Download one package to a local file.
    pub async fn fetch_package(
        &self,
        game_id: &str,
        stamp: &str,
        into: &Path,
        timeout: Duration,
    ) -> Result<(), SyncError> {
        let remote = package_remote(&self.settings, game_id, stamp);
        let args = copyto_args(&remote, &into.to_string_lossy());
        self.run(&args, timeout).await.map(|_| ())
    }

    /// Upload one local file as this game's package.
    pub async fn send_package(
        &self,
        game_id: &str,
        stamp: &str,
        from: &Path,
        timeout: Duration,
    ) -> Result<(), SyncError> {
        let remote = package_remote(&self.settings, game_id, stamp);
        let args = copyto_args(&from.to_string_lossy(), &remote);
        self.run(&args, timeout).await.map(|_| ())
    }

    /// Delete one version package.
    async fn remove_package(&self, game_id: &str, stamp: &str) -> Result<(), SyncError> {
        let remote = package_remote(&self.settings, game_id, stamp);
        let args = deletefile_args(&remote);
        self.run(&args, COMMAND_TIMEOUT).await.map(|_| ())
    }
}

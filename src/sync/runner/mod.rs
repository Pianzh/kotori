//! Driving rclone: the orchestration half of cloud sync.
//!
//! [`super`] builds argument lists; this module runs them, resolves what each
//! save location means on *this* machine, and reports per-location outcomes
//! that the UI and CLI can show verbatim.
//!
//! Three rules shape everything here (ADR-010 / ADR-011):
//!   * **Never destroy local data.** Uploads use `copy` (which cannot delete at
//!     the destination), the automatic pre-launch pull uses `--update` so a
//!     newer local save always survives, and pruning only ever touches snapshot
//!     directories in the cloud.
//!   * **Secrets never touch a disk or a command line.** They are read from the
//!     keyring and handed to the child through its environment.
//!   * **A failure says what failed.** Every location gets its own outcome, so
//!     "synced" is never reported for something that was skipped.
//!
//! 文件分工：本文件是 [`Runner`] 本身——超时预算、"跑一次 rclone"的传输底座，
//! 以及每次操作的汇报类型（[`GameOutcome`] / [`LocationOutcome`]）；
//! `upload.rs` 管上传与启动前拉取，`restore.rs` 管恢复与保留窗口，
//! `diagnostics.rs` 把 rclone 的 stderr 翻成人话，`testing.rs` 是测试用的假 rclone。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

use super::{
    CURRENT_DIR, SaveTarget, SyncError, game_remote, is_snapshot, list_dirs_args, parse_dirs,
    prune_plan, push_excludes, rclone_env, remote_root, validate, validate_secrets,
    versions_remote,
};
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};

use self::diagnostics::explain_failure;

mod diagnostics;
mod restore;
#[cfg(test)]
mod testing;
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

/// What happened to one save location.
#[derive(Debug, Clone, Serialize)]
pub struct LocationOutcome {
    /// The location as configured (portable form).
    pub configured: String,
    /// Where it resolved to on this machine.
    pub local: String,
    /// `uploaded` / `pulled` / `restored` / `skipped` / `failed`.
    pub action: &'static str,
    /// One line explaining the action, safe to show to the user.
    pub detail: String,
}

impl LocationOutcome {
    fn new(target: &SaveTarget, action: &'static str, detail: impl Into<String>) -> Self {
        Self {
            configured: target.configured.clone(),
            local: target.local.to_string_lossy().to_string(),
            action,
            detail: detail.into(),
        }
    }

    pub fn ok(&self) -> bool {
        self.action != "failed"
    }
}

/// Result of one operation on one game.
#[derive(Debug, Clone, Serialize)]
pub struct GameOutcome {
    pub game_id: String,
    pub name: String,
    pub ok: bool,
    pub locations: Vec<LocationOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl GameOutcome {
    pub fn failed(game_id: &str, name: &str, error: impl Into<String>) -> Self {
        Self {
            game_id: game_id.to_string(),
            name: name.to_string(),
            ok: false,
            locations: Vec::new(),
            error: Some(error.into()),
        }
    }

    fn from_locations(game_id: &str, name: &str, locations: Vec<LocationOutcome>) -> Self {
        let error = locations
            .iter()
            .find(|o| !o.ok())
            .map(|o| format!("{}: {}", o.configured, o.detail));
        Self {
            game_id: game_id.to_string(),
            name: name.to_string(),
            ok: error.is_none(),
            locations,
            error,
        }
    }
}

/// Runs rclone against one sync configuration.
pub struct Runner {
    rclone: PathBuf,
    settings: SyncConfig,
    keyring: Keyring,
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
        }
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

    /// Run rclone without any credentials — only `obscure` works this way.
    async fn run_bare(&self, args: &[String], timeout: Duration) -> Result<String, SyncError> {
        self.run_with(args, timeout, Vec::new()).await
    }

    async fn run_with(
        &self,
        args: &[String],
        timeout: Duration,
        env: Vec<(String, String)>,
    ) -> Result<String, SyncError> {
        let mut command = tokio::process::Command::new(&self.rclone);
        command
            .args(args)
            .envs(env)
            // Even the "no credentials" path must ignore the user's config.
            .env("RCLONE_CONFIG", super::null_config_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A timed-out transfer must not keep running in the background.
            .kill_on_drop(true);

        let output = tokio::time::timeout(timeout, command.output())
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
    /// backups (ADR-010). rclone only accepts this value as an argument, so the
    /// password is briefly visible in `ps` during this one-off setup call —
    /// accepted in exchange for never deriving the key ourselves. Sync runs
    /// themselves read the obscured form from the keyring and put nothing on a
    /// command line.
    pub async fn obscure(&self, password: &str) -> Result<String, SyncError> {
        let output = self
            .run_bare(&super::obscure_args(password), COMMAND_TIMEOUT)
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

    /// The save-location directories that already exist in the cloud.
    pub async fn current_keys(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let remote = format!("{}/{CURRENT_DIR}", game_remote(&self.settings, game_id));
        let output = self.run(&list_dirs_args(&remote), COMMAND_TIMEOUT).await?;
        Ok(parse_dirs(&output))
    }

    /// Snapshot stamps present in the cloud, oldest first.
    pub async fn versions(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let remote = versions_remote(&self.settings, game_id);
        let output = self.run(&list_dirs_args(&remote), COMMAND_TIMEOUT).await?;
        // Only ever report our own snapshot directories.
        Ok(parse_dirs(&output)
            .into_iter()
            .filter(|name| super::is_snapshot(name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::runner::testing::FakeRclone;

    #[tokio::test]
    async fn credentials_reach_rclone_through_the_environment_only() {
        let fake = FakeRclone::new("secrets");
        let outcome = fake.runner(false, 0).check().await.unwrap();
        assert_eq!(outcome, "kotori:bkt/prefix");

        let calls = fake.calls();
        assert!(calls[0].starts_with("mkdir kotori:bkt/prefix"), "{calls:?}");
        // Nothing secret may ever appear on a command line: `ps` is world-read.
        for call in &calls {
            assert!(!call.contains("appkey456"), "{call}");
            assert!(!call.contains("keyid123"), "{call}");
        }

        let env = fake.env_log();
        assert!(
            env.contains("env:RCLONE_CONFIG_KOTORI_ACCOUNT=keyid123"),
            "{env}"
        );
        assert!(
            env.contains("env:RCLONE_CONFIG_KOTORI_KEY=appkey456"),
            "{env}"
        );
        assert!(env.contains("env:RCLONE_CONFIG=/dev/null"), "{env}");
        // Unencrypted setups carry no crypt remote at all.
        assert!(!env.contains("KOTORIENC"), "{env}");
    }

    #[tokio::test]
    async fn encrypted_setups_hand_over_only_the_obscured_password() {
        let fake = FakeRclone::new("encrypted");
        fake.runner(true, 0).check().await.unwrap();

        let env = fake.env_log();
        assert!(
            env.contains("env:RCLONE_CONFIG_KOTORIENC_TYPE=crypt"),
            "{env}"
        );
        assert!(
            env.contains("env:RCLONE_CONFIG_KOTORIENC_PASSWORD=obscured-blob"),
            "{env}"
        );
        assert!(!env.contains("hunter2"), "{env}");
    }

    #[tokio::test]
    async fn missing_credentials_stop_the_run_before_rclone_is_started() {
        let fake = FakeRclone::new("no-secrets");
        let runner = Runner::with_binary(&fake.bin, fake.settings(false, 0), Keyring::memory());
        let outcome = runner.upload("demo", "Demo", &[]).await;

        assert!(!outcome.ok);
        assert!(outcome.error.unwrap().contains("B2 凭据"));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_missing_endpoint_means_rclone_picks_one() {
        // The native B2 backend is happy with no endpoint, and that is the
        // normal case; the S3 endpoint the B2 console shows is a different API
        // and is rejected before a run ever starts (see `sync::validate`).
        let fake = FakeRclone::new("no-endpoint");
        let settings = fake.settings(false, 0);
        assert!(settings.endpoint.is_empty());
        assert!(crate::sync::validate(&settings).is_ok());
    }

    #[tokio::test]
    async fn obscuring_is_left_to_rclone_and_stored_in_both_forms() {
        let fake = FakeRclone::new("obscure");
        let runner = fake.runner(true, 0);
        assert_eq!(runner.obscure("hunter2").await.unwrap(), "obscured-blob");

        let calls = fake.calls();
        assert!(calls[0].starts_with("obscure hunter2"), "{calls:?}");
        // No credentials are needed to obscure, and the user's own rclone.conf
        // must not be consulted even here.
        assert!(!fake.env_log().contains("RCLONE_CONFIG_KOTORI_KEY="));
        assert!(fake.env_log().contains("env:RCLONE_CONFIG=/dev/null"));
    }

    #[tokio::test]
    async fn a_disabled_sync_config_is_refused() {
        let fake = FakeRclone::new("disabled");
        let mut settings = fake.settings(false, 0);
        settings.enabled = false;
        let runner = Runner::with_binary(&fake.bin, settings, fake.keyring(false));

        assert!(matches!(runner.ready(), Err(SyncError::NotEnabled)));
        assert!(fake.calls().is_empty());
    }
}

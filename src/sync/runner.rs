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

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

use super::{
    CURRENT_DIR, Merge, SaveTarget, SyncError, copy_args, game_remote, list_dirs_args, parse_dirs,
    purge_args, rclone_env, remote_root, restore_args, validate, validate_secrets, version_stamp,
    versions_remote,
};
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};

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

    /// Push every location of a game into the cloud.
    pub async fn upload(&self, game_id: &str, name: &str, targets: &[SaveTarget]) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let stamp = version_stamp(chrono::Utc::now());
        let mut outcomes = Vec::with_capacity(targets.len());

        for target in targets {
            if !target.local.is_dir() {
                outcomes.push(LocationOutcome::new(
                    target,
                    "skipped",
                    "本地没有这个目录，没什么可上传的",
                ));
                continue;
            }

            let destination = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            // Replaced files land under their own snapshot, per location, so two
            // locations in the same game can never overwrite each other there.
            let backup = format!(
                "{}/{stamp}/{}",
                versions_remote(&self.settings, game_id),
                target.key
            );

            let mut args = copy_args(
                &target.local.to_string_lossy(),
                &destination,
                Some(&backup),
                Merge::Replace,
            );
            super::push_excludes(&mut args, &target.exclude);

            match self.run(&args, COMMAND_TIMEOUT).await {
                Ok(_) => outcomes.push(LocationOutcome::new(target, "uploaded", "已上传")),
                Err(error) => {
                    outcomes.push(LocationOutcome::new(target, "failed", error.to_string()))
                }
            }
        }

        // Retention is a separate, best-effort step: failing to tidy up must
        // never turn a successful upload into a failure.
        if self.settings.keep_versions > 0
            && let Err(error) = self.prune(game_id).await
        {
            tracing::warn!("{}: 清理旧快照失败: {error}", game_id);
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Fetch anything that is *newer* in the cloud, keeping newer local files.
    ///
    /// Used before a launch. `Merge::Newer` means a local save that was never
    /// uploaded (because the last upload failed) survives this.
    pub async fn pull(&self, game_id: &str, name: &str, targets: &[SaveTarget]) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let available = match self.current_keys(game_id).await {
            Ok(keys) => keys,
            // Nothing has ever been uploaded: not an error, just nothing to do.
            Err(error) => return GameOutcome::failed(game_id, name, error.to_string()),
        };

        let mut outcomes = Vec::with_capacity(targets.len());
        for target in targets {
            if !available.contains(&target.key) {
                outcomes.push(LocationOutcome::new(
                    target,
                    "skipped",
                    "云端还没有这个位置的存档",
                ));
                continue;
            }

            let source = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            let mut args = copy_args(&source, &target.local.to_string_lossy(), None, Merge::Newer);
            super::push_excludes(&mut args, &target.exclude);

            match self.run(&args, COMMAND_TIMEOUT).await {
                Ok(_) => outcomes.push(LocationOutcome::new(
                    target,
                    "pulled",
                    "已取回云端较新的文件",
                )),
                Err(error) => {
                    outcomes.push(LocationOutcome::new(target, "failed", error.to_string()))
                }
            }
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Restore a game's saves.
    ///
    /// `version = None` restores the newest state. Naming a snapshot restores
    /// the state as it was *before* that upload: the snapshot holds the files
    /// that were replaced at the time, so it is overlaid on top of the current
    /// copy to rebuild that point in time.
    ///
    /// Before overwriting anything, the current local state is uploaded as a
    /// fresh snapshot (best effort). A restore is therefore itself undoable.
    pub async fn restore(
        &self,
        game_id: &str,
        name: &str,
        targets: &[SaveTarget],
        version: Option<&str>,
    ) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }
        if let Some(version) = version
            && !super::is_snapshot(version)
        {
            return GameOutcome::failed(
                game_id,
                name,
                format!("不是合法的快照名: {version}（形如 20260911T101500Z）"),
            );
        }

        // Keep what we are about to replace.
        if let Err(error) = self.snapshot_now(game_id, targets).await {
            tracing::warn!("{}: 恢复前快照失败（继续恢复）: {error}", game_id);
        }

        let available = match self.current_keys(game_id).await {
            Ok(keys) => keys,
            Err(error) => return GameOutcome::failed(game_id, name, error.to_string()),
        };

        let mut outcomes = Vec::with_capacity(targets.len());
        for target in targets {
            if !available.contains(&target.key) {
                outcomes.push(LocationOutcome::new(
                    target,
                    "skipped",
                    "云端还没有这个位置的存档",
                ));
                continue;
            }

            let current = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            let mut args = restore_args(&current, &target.local.to_string_lossy());
            super::push_excludes(&mut args, &target.exclude);

            if let Err(error) = self.run(&args, COMMAND_TIMEOUT).await {
                outcomes.push(LocationOutcome::new(target, "failed", error.to_string()));
                continue;
            }

            // Overlay the snapshot to get back to that point in time.
            if let Some(version) = version {
                let snapshot = format!(
                    "{}/{version}/{}",
                    versions_remote(&self.settings, game_id),
                    target.key
                );
                let mut args = restore_args(&snapshot, &target.local.to_string_lossy());
                super::push_excludes(&mut args, &target.exclude);
                if let Err(error) = self.run(&args, COMMAND_TIMEOUT).await {
                    outcomes.push(LocationOutcome::new(
                        target,
                        "failed",
                        format!("快照 {version} 叠加失败: {error}"),
                    ));
                    continue;
                }
                outcomes.push(LocationOutcome::new(
                    target,
                    "restored",
                    format!("已恢复到快照 {version}"),
                ));
            } else {
                outcomes.push(LocationOutcome::new(target, "restored", "已恢复到最新备份"));
            }
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Upload the current state without touching the retention window.
    ///
    /// Used before a restore so the state being replaced is recoverable. It
    /// deliberately does not reuse [`Self::upload`]: that one prunes, and a
    /// restore must not be able to expire a snapshot as a side effect.
    async fn snapshot_now(&self, game_id: &str, targets: &[SaveTarget]) -> Result<(), SyncError> {
        let stamp = version_stamp(chrono::Utc::now());
        for target in targets {
            if !target.local.is_dir() {
                continue;
            }
            let destination = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            let backup = format!(
                "{}/{stamp}/{}",
                versions_remote(&self.settings, game_id),
                target.key
            );
            let mut args = copy_args(
                &target.local.to_string_lossy(),
                &destination,
                Some(&backup),
                Merge::Replace,
            );
            super::push_excludes(&mut args, &target.exclude);
            self.run(&args, COMMAND_TIMEOUT).await?;
        }
        Ok(())
    }

    /// Delete the snapshots that fall outside the retention window.
    ///
    /// Never touches local files, and never touches a cloud directory that does
    /// not look like one of our own snapshots.
    pub async fn prune(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let stamps = self.versions(game_id).await?;
        let doomed = super::prune_plan(&stamps, self.settings.keep_versions);
        if doomed.is_empty() {
            return Ok(doomed);
        }

        let root = versions_remote(&self.settings, game_id);
        for stamp in &doomed {
            let args = purge_args(&format!("{root}/{stamp}"));
            self.run(&args, COMMAND_TIMEOUT).await?;
            tracing::info!("{game_id}: 已删除旧快照 {stamp}");
        }
        Ok(doomed)
    }
}

/// Turn rclone's stderr into something the user can act on.
///
/// The common failures are all "the setup is not right yet", and rclone's own
/// wording ("failed to authenticate: Unknown 401  (401 bad_auth_token)") does
/// not say which of the three values to go and check. The original text is kept
/// so nothing is hidden from the user.
fn explain_failure(stderr: &str) -> String {
    let detail = clean_stderr(stderr);
    let lower = detail.to_lowercase();

    let hint = if lower.contains("bad_auth_token")
        || lower.contains("401")
        || lower.contains("unauthorized")
    {
        Some(
            "B2 不认这组凭据。检查 keyID 是不是 Application Key ID（形如 005a…，不是账号 ID），\
             以及 applicationKey 有没有完整复制",
        )
    } else if lower.contains("403") || lower.contains("forbidden") || lower.contains("not allowed")
    {
        Some(
            "凭据有效，但这个 key 没有这个 bucket 的权限。创建 Application Key 时要勾上该 bucket，\
             并把 Type of Access 选成 Read and Write",
        )
    } else if lower.contains("bucket")
        && (lower.contains("not found")
            || lower.contains("does not exist")
            || lower.contains("no such"))
    {
        Some("找不到这个 bucket：检查名字有没有写错，以及 Application Key 是否授权了它")
    } else if lower.contains("no such host")
        || lower.contains("connection refused")
        || lower.contains("timeout")
        || lower.contains("dial tcp")
        || lower.contains("tls")
    {
        Some("连不上 B2：检查网络、代理或 DNS 设置")
    } else {
        None
    };

    match hint {
        Some(hint) => format!("{hint}\n（rclone 原话：{detail}）"),
        None => detail,
    }
}

/// The last non-empty line of rclone's output, with its log prefix removed.
fn clean_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rfind(|_| true)
        .map(strip_rclone_log_prefix)
        .unwrap_or_default()
}

/// `2026/09/11 23:54:35 CRITICAL: message` -> `message`.
fn strip_rclone_log_prefix(line: &str) -> String {
    let mut rest = line.trim();
    let starts_with_timestamp = rest.len() > 20
        && rest.is_char_boundary(20)
        && rest[..10].chars().all(|c| c.is_ascii_digit() || c == '/')
        && rest[10..11] == *" "
        && rest[11..19].chars().all(|c| c.is_ascii_digit() || c == ':');
    if starts_with_timestamp {
        rest = rest[20..].trim_start();
    }
    for level in [
        "CRITICAL: ",
        "ERROR : ",
        "ERROR: ",
        "WARNING: ",
        "NOTICE: ",
        "INFO  : ",
        "INFO : ",
        "DEBUG : ",
    ] {
        if let Some(tail) = rest.strip_prefix(level) {
            return tail.trim().to_string();
        }
    }
    rest.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in `rclone` that records how it was called.
    ///
    /// It never touches the network, so the tests pin the *contract* — which
    /// arguments kotori builds, and with which credentials in the environment —
    /// rather than rclone's own behaviour.
    struct FakeRclone {
        dir: PathBuf,
        bin: PathBuf,
    }

    impl FakeRclone {
        fn new(tag: &str) -> Self {
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
  echo "obscured-blob"
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

        fn settings(&self, encryption: bool, keep_versions: u32) -> SyncConfig {
            SyncConfig {
                enabled: true,
                endpoint: String::new(),
                bucket: "bkt".to_string(),
                prefix: "prefix".to_string(),
                encryption,
                keep_versions,
            }
        }

        fn keyring(&self, encryption: bool) -> Keyring {
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

        fn runner(&self, encryption: bool, keep_versions: u32) -> Runner {
            Runner::with_binary(
                &self.bin,
                self.settings(encryption, keep_versions),
                self.keyring(encryption),
            )
        }

        /// Teach the fake what `lsf` should print for a remote path.
        fn set_listing(&self, remote: &str, entries: &[&str]) {
            let key: String = remote
                .chars()
                .map(|c| if c == '/' || c == ':' { '_' } else { c })
                .collect();
            let body: String = entries.iter().map(|e| format!("{e}/\n")).collect();
            std::fs::write(self.dir.join("lsf").join(key), body).unwrap();
        }

        fn fail_on(&self, needle: &str) {
            std::fs::write(self.dir.join("fail"), needle).unwrap();
        }

        /// Every rclone invocation, in order.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("log"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.strip_prefix("argv:"))
                .map(str::to_string)
                .collect()
        }

        fn env_log(&self) -> String {
            std::fs::read_to_string(self.dir.join("log")).unwrap_or_default()
        }

        fn calls_matching(&self, needle: &str) -> Vec<String> {
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

    fn target(dir: &std::path::Path, configured: &str, key: &str) -> SaveTarget {
        SaveTarget {
            key: key.to_string(),
            configured: configured.to_string(),
            local: dir.to_path_buf(),
            exclude: Vec::new(),
        }
    }

    const CURRENT: &str = "kotori:bkt/prefix/games/demo/current";
    const VERSIONS: &str = "kotori:bkt/prefix/games/demo/versions";

    #[tokio::test]
    async fn upload_sends_each_location_into_its_own_cloud_directory() {
        let fake = FakeRclone::new("upload");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(saves.join("nested")).unwrap();

        let mut target = target(&saves, "savedata", "rel-savedata");
        target.exclude = vec!["*.log".to_string(), "  ".to_string()];
        let outcome = fake
            .runner(false, 0)
            .upload("demo", "Demo", std::slice::from_ref(&target))
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "uploaded");

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let call = &calls[0];
        assert!(call.starts_with("copy "), "{call}");
        assert!(
            call.contains(&format!("copy {} {CURRENT}/rel-savedata", saves.display())),
            "{call}"
        );
        // Replaced files go aside so we keep history without a repo format.
        assert!(
            call.contains(&format!("--backup-dir {VERSIONS}/")),
            "{call}"
        );
        assert!(
            call.contains("/rel-savedata "),
            "per-location snapshot: {call}"
        );
        assert!(
            call.contains("--suffix  "),
            "empty suffix keeps names: {call}"
        );
        assert!(call.contains("--exclude *.log"), "{call}");
        assert!(
            !call.contains("--update"),
            "an upload must overwrite: {call}"
        );
        assert!(!call.contains("--delete"), "{call}");
        // Blank patterns are dropped rather than sent to rclone.
        assert!(!call.contains("--exclude   "), "{call}");
    }

    #[tokio::test]
    async fn a_location_that_does_not_exist_locally_is_reported_not_ignored() {
        let fake = FakeRclone::new("missing");
        let absent = fake.dir.join("never-created");
        let outcome = fake
            .runner(false, 0)
            .upload(
                "demo",
                "Demo",
                &[target(&absent, "savedata", "rel-savedata")],
            )
            .await;

        assert!(
            outcome.ok,
            "nothing to upload is not a failure: {outcome:?}"
        );
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(fake.calls().is_empty(), "no rclone call was needed");
    }

    #[tokio::test]
    async fn a_failing_location_is_named_and_does_not_hide_the_others() {
        let fake = FakeRclone::new("partial");
        let first = fake.dir.join("one");
        let second = fake.dir.join("two");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        fake.fail_on(&format!("copy {} ", first.display()));

        let outcome = fake
            .runner(false, 0)
            .upload(
                "demo",
                "Demo",
                &[
                    target(&first, "one", "rel-one"),
                    target(&second, "two", "rel-two"),
                ],
            )
            .await;

        assert!(!outcome.ok);
        assert_eq!(outcome.locations[0].action, "failed");
        assert_eq!(
            outcome.locations[1].action, "uploaded",
            "one bad location must not abort the rest"
        );
        assert!(outcome.error.unwrap().contains("one"));
    }

    #[tokio::test]
    async fn pulling_before_a_launch_can_never_overwrite_a_newer_local_save() {
        let fake = FakeRclone::new("pull");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "pulled");

        let call = &fake.calls()[1];
        assert!(call.contains(&format!("{CURRENT}/rel-savedata {}", saves.display())));
        // The whole point: an upload that failed earlier means the local copy is
        // newer, and it must win.
        assert!(call.contains("--update"), "{call}");
        assert!(
            !call.contains("--backup-dir"),
            "a pull must not create local versions: {call}"
        );
    }

    #[tokio::test]
    async fn a_game_the_cloud_has_never_seen_is_not_an_error() {
        let fake = FakeRclone::new("pull-empty");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(false, 0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(outcome.locations[0].detail.contains("云端还没有"));
        assert_eq!(fake.calls().len(), 1, "only the listing happened");
    }

    #[tokio::test]
    async fn restoring_snapshots_what_it_is_about_to_replace() {
        let fake = FakeRclone::new("restore");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                None,
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "restored");

        let calls = fake.calls();
        // 1: the safety snapshot, 2: the listing, 3: the restore itself.
        assert!(
            calls[0].contains("--backup-dir"),
            "snapshot first: {calls:?}"
        );
        assert!(calls[1].starts_with("lsf"), "{calls:?}");
        assert!(
            calls[2].contains(&format!("{CURRENT}/rel-savedata {}", saves.display())),
            "{calls:?}"
        );
        assert!(
            !calls[2].contains("--update"),
            "an explicit restore is meant to win: {calls:?}"
        );
    }

    #[tokio::test]
    async fn restoring_a_snapshot_overlays_it_on_the_newest_state() {
        let fake = FakeRclone::new("restore-version");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("20260911T101500Z"),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert!(outcome.locations[0].detail.contains("20260911T101500Z"));

        let calls = fake.calls();
        // Current first, then the snapshot that holds the replaced files: a
        // snapshot on its own is only the diff of one upload.
        let current = calls
            .iter()
            .position(|c| c.contains(&format!("{CURRENT}/rel-savedata ")))
            .expect("current copy");
        let snapshot = calls
            .iter()
            .position(|c| c.contains(&format!("{VERSIONS}/20260911T101500Z/rel-savedata ")))
            .expect("snapshot overlay");
        assert!(current < snapshot, "{calls:?}");
    }

    #[tokio::test]
    async fn a_bogus_snapshot_name_is_refused_before_anything_is_touched() {
        let fake = FakeRclone::new("restore-bogus");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("../../../etc"),
            )
            .await;

        assert!(!outcome.ok);
        assert!(outcome.error.unwrap().contains("不是合法的快照名"));
        assert!(
            fake.calls().is_empty(),
            "nothing may run: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn retention_only_ever_purges_our_own_old_snapshots() {
        let fake = FakeRclone::new("prune");
        fake.set_listing(
            VERSIONS,
            &[
                "20260903T000000Z",
                "20260901T000000Z",
                "20260902T000000Z",
                "current",
                "not-ours",
            ],
        );

        let removed = fake.runner(false, 2).prune("demo").await.unwrap();
        assert_eq!(removed, vec!["20260901T000000Z".to_string()]);

        let purges = fake.calls_matching("purge");
        assert_eq!(purges.len(), 1, "{purges:?}");
        assert!(purges[0].contains(&format!("{VERSIONS}/20260901T000000Z")));
        assert!(
            !fake.env_log().is_empty(),
            "pruning still needs credentials in the environment"
        );
    }

    #[tokio::test]
    async fn retention_keeps_everything_unless_the_user_asked_otherwise() {
        let fake = FakeRclone::new("prune-off");
        fake.set_listing(VERSIONS, &["20260901T000000Z", "20260902T000000Z"]);

        assert!(
            fake.runner(false, 0)
                .prune("demo")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(fake.calls_matching("purge").is_empty());
        // Even with the window off, no listing is needed if keep is 0 — but if
        // it is, it must not delete anything it does not recognise.
        assert!(
            fake.runner(false, 5)
                .prune("demo")
                .await
                .unwrap()
                .is_empty()
        );
    }

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
    fn rclone_failures_are_explained_in_terms_of_what_to_check() {
        // Captured from the real rclone against B2 with made-up credentials.
        let unauthorized = "2026/09/11 23:51:56 CRITICAL: Failed to create file system for \
\"kotori:kotori-saves/kotori\": failed to authorize account: failed to authenticate: \
Unknown 401  (401 bad_auth_token)";
        let explained = explain_failure(unauthorized);
        assert!(explained.contains("Application Key ID"), "{explained}");
        assert!(
            explained.contains("bad_auth_token"),
            "the original must survive: {explained}"
        );
        assert!(
            !explained.contains("CRITICAL"),
            "log noise is stripped: {explained}"
        );

        let forbidden = "2026/09/11 10:00:00 ERROR : bucket is not allowed: 403 forbidden";
        assert!(explain_failure(forbidden).contains("Read and Write"));

        let missing = "2026/09/11 10:00:00 CRITICAL: bucket kotori-saves not found";
        assert!(explain_failure(missing).contains("bucket"));

        let offline =
            "2026/09/11 10:00:00 CRITICAL: dial tcp: lookup api.backblazeb2.com: no such host";
        assert!(explain_failure(offline).contains("网络"));

        // Anything unrecognised is passed through as-is, minus the log prefix.
        let other = "2026/09/11 10:00:00 NOTICE: something else happened";
        assert_eq!(explain_failure(other), "something else happened");
        assert_eq!(clean_stderr("\n\n"), "");
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

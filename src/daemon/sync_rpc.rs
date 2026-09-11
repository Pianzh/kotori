//! `sync.*` RPCs and the session hooks that drive save sync.
//!
//! Kept out of `daemon/mod.rs` because it is a self-contained concern: the
//! daemon owns the config and the keyring, and everything here is either a
//! request from a client or a reaction to a session event.
//!
//! Two rules the code below is careful about (ADR-010):
//!   * **a secret is never echoed back.** `sync.status` reports *which* secrets
//!     exist and how the user can read them without kotori; it never returns a
//!     value, not even to the local UI.
//!   * **sync never blocks a game.** The pre-launch pull has a deadline and its
//!     failure is reported, not raised: the user asked to play, not to sync.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Daemon;
use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};
use crate::sync::{
    self, SaveTarget,
    runner::{GameOutcome, PULL_TIMEOUT, Runner, SETTLE_DELAY},
};

/// Ceiling for the sliding window. A typo here decides how much history is
/// deleted, so the range is deliberately small and explicit.
pub const MAX_KEEP_VERSIONS: u32 = 100;

/// Keyring handle plus the last result per game, for the settings page.
pub(super) struct SyncState {
    keyring: Keyring,
    records: Mutex<HashMap<String, SyncRecord>>,
}

impl SyncState {
    /// The system keyring, or a session-only store when there is none.
    pub(super) fn system() -> Self {
        let (keyring, _ephemeral) = Keyring::system_or_memory();
        Self::with_keyring(keyring)
    }

    pub(super) fn with_keyring(keyring: Keyring) -> Self {
        Self {
            keyring,
            records: Mutex::new(HashMap::new()),
        }
    }

    fn remember(&self, game_id: &str, action: &str, outcome: &GameOutcome) {
        let detail = outcome.error.clone().unwrap_or_else(|| {
            let done = outcome
                .locations
                .iter()
                .filter(|l| l.action != "skipped")
                .count();
            if done == 0 {
                "没有需要同步的变化".to_string()
            } else {
                format!("{done} 个位置已{action}")
            }
        });
        if let Ok(mut records) = self.records.lock() {
            records.insert(
                game_id.to_string(),
                SyncRecord {
                    at: chrono::Utc::now(),
                    ok: outcome.ok,
                    action: action.to_string(),
                    detail,
                },
            );
        }
    }
}

/// When a game was last synced, and how it went.
#[derive(Debug, Clone, Serialize)]
pub struct SyncRecord {
    pub at: chrono::DateTime<chrono::Utc>,
    pub ok: bool,
    pub action: String,
    pub detail: String,
}

/// Fields a client may change on the sync settings.
#[derive(Debug, Default, Deserialize)]
pub(super) struct SettingsPatch {
    enabled: Option<bool>,
    endpoint: Option<String>,
    bucket: Option<String>,
    prefix: Option<String>,
    encryption: Option<bool>,
    keep_versions: Option<u32>,
    /// Required to change a setting that would make existing cloud data
    /// unreadable (turning encryption on or off).
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Credentials {
    #[serde(default)]
    key_id: String,
    #[serde(default)]
    app_key: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Password {
    #[serde(default)]
    password: String,
    #[serde(default)]
    force: bool,
}

impl Daemon {
    /// Build a runner for the current settings, or explain why we cannot.
    fn sync_runner(&self, settings: &SyncConfig) -> Result<Runner, String> {
        Runner::new(settings.clone(), self.sync.keyring.clone()).map_err(|e| e.to_string())
    }

    /// The name and resolved locations of a game that is ready to sync.
    async fn sync_targets(&self, game_id: &str) -> Result<(String, Vec<SaveTarget>), String> {
        let config = self.config.read().await;
        let game = config
            .games
            .get(game_id)
            .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
        if game.save_paths.is_empty() {
            return Err(format!("「{}」还没有配置存档位置", game.name));
        }
        let targets = sync::targets(game, &config)?;
        Ok((game.name.clone(), targets))
    }

    /// Everything the settings page needs, with no secret values in it.
    pub(super) async fn rpc_sync_status(&self) -> Result<Value, String> {
        let config = self.config.read().await;
        let settings = config.sync.clone();
        let rclone = sync::find_rclone().map(|p| p.to_string_lossy().to_string());
        let secrets: Vec<&str> = self
            .sync
            .keyring
            .present()
            .into_iter()
            .map(|key| key.account())
            .collect();

        // Only meaningful once sync is on; an unfinished setup is not an error
        // while the user is still typing.
        let problem = if settings.enabled {
            sync::validate(&settings)
                .err()
                .or_else(|| sync::validate_secrets(&settings, &self.sync.keyring).err())
                .map(|error| error.to_string())
        } else {
            None
        };

        let records = self
            .sync
            .records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default();

        let mut games: Vec<Value> = config
            .games
            .iter()
            .map(|(id, game)| {
                // Report a location that cannot be resolved *now* (an unplugged
                // disk, a removed prefix) instead of failing later at sync time.
                let (count, location_problem) = match sync::targets(game, &config) {
                    Ok(targets) => (targets.len(), Value::Null),
                    Err(error) => (0, Value::String(error)),
                };
                json!({
                    "id": id,
                    "name": game.name,
                    "locations": count,
                    "location_problem": location_problem,
                    "last": records.get(id),
                })
            })
            .collect();
        games.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["name"].as_str().unwrap_or_default())
        });

        Ok(json!({
            "settings": settings,
            "enabled": settings.enabled,
            "rclone": rclone,
            "keyring": {
                "backend": self.sync.keyring.describe(),
                "ephemeral": self.sync.keyring.is_ephemeral(),
            },
            // Which entries exist — never what they contain.
            "secrets": secrets,
            "ready": settings.enabled && problem.is_none() && rclone.is_some(),
            "problem": problem,
            "remote": sync::remote_root(&settings),
            "password_hint": crate::secrets::lookup_hint(SecretKey::SyncPassword),
            "pull_timeout_secs": PULL_TIMEOUT.as_secs(),
            "keep_versions_max": MAX_KEEP_VERSIONS,
            "games": games,
        }))
    }

    /// Change the non-secret sync settings.
    pub(super) async fn rpc_sync_set_settings(
        &self,
        patch: SettingsPatch,
    ) -> Result<Value, String> {
        let keyring = self.sync.keyring.clone();
        self.mutate_config(|config| {
            let before = config.sync.clone();
            let mut candidate = before.clone();

            if let Some(enabled) = patch.enabled {
                candidate.enabled = enabled;
            }
            if let Some(value) = &patch.endpoint {
                candidate.endpoint = clean_endpoint(value)?;
            }
            if let Some(value) = &patch.bucket {
                candidate.bucket = clean_bucket(value)?;
            }
            if let Some(value) = &patch.prefix {
                candidate.prefix = clean_prefix(value)?;
            }
            if let Some(keep) = patch.keep_versions {
                if keep > MAX_KEEP_VERSIONS {
                    return Err(format!("保留版本数最多 {MAX_KEEP_VERSIONS}（当前 {keep}）"));
                }
                candidate.keep_versions = keep;
            }

            if let Some(encryption) = patch.encryption
                && encryption != before.encryption
            {
                // Flipping this changes how every existing file in the bucket is
                // read: on becomes garbage, off becomes unreadable. The user has
                // to mean it.
                if !patch.force {
                    return Err(if encryption {
                        "开启加密会让 bucket 里已有的明文存档读不出来（除非换一个 prefix）。\
                         确认要开启请再确认一次"
                            .to_string()
                    } else {
                        "关闭加密后，之前加密上传的存档将无法解密。确认要关闭请再确认一次"
                            .to_string()
                    });
                }
                if encryption && !matches!(keyring.get(SecretKey::SyncPassword), Ok(Some(_))) {
                    return Err("开启加密前请先在设置页里设定同步密码".to_string());
                }
                candidate.encryption = encryption;
            }

            if candidate.enabled {
                sync::validate(&candidate).map_err(|e| e.to_string())?;
            }

            config.sync = candidate.clone();
            tracing::info!(
                "sync settings updated (enabled={}, encryption={}, keep_versions={})",
                candidate.enabled,
                candidate.encryption,
                candidate.keep_versions
            );
            Ok(json!({ "settings": candidate }))
        })
        .await
    }

    /// Store (or clear) the B2 credentials.
    pub(super) fn rpc_sync_set_credentials(
        &self,
        credentials: Credentials,
    ) -> Result<Value, String> {
        let key_id = credentials.key_id.trim();
        let app_key = credentials.app_key.trim();

        if key_id.is_empty() && app_key.is_empty() {
            self.sync
                .keyring
                .clear(SecretKey::B2KeyId)
                .map_err(|e| e.to_string())?;
            self.sync
                .keyring
                .clear(SecretKey::B2AppKey)
                .map_err(|e| e.to_string())?;
            return Ok(json!({ "cleared": true }));
        }

        if key_id.is_empty() || app_key.is_empty() {
            return Err("key id 和 application key 要一起填".to_string());
        }

        self.sync
            .keyring
            .set(SecretKey::B2KeyId, key_id)
            .map_err(|e| e.to_string())?;
        self.sync
            .keyring
            .set(SecretKey::B2AppKey, app_key)
            .map_err(|e| e.to_string())?;
        tracing::info!("B2 credentials stored in the keyring");
        Ok(json!({ "stored": true }))
    }

    /// Store (or clear) the sync password, in both the readable and rclone form.
    pub(super) async fn rpc_sync_set_password(&self, password: Password) -> Result<Value, String> {
        if password.password.is_empty() {
            self.sync
                .keyring
                .clear(SecretKey::SyncPassword)
                .map_err(|e| e.to_string())?;
            self.sync
                .keyring
                .clear(SecretKey::SyncPasswordObscured)
                .map_err(|e| e.to_string())?;
            return Ok(json!({ "cleared": true }));
        }

        let settings = self.config.read().await.sync.clone();
        let existing = matches!(self.sync.keyring.get(SecretKey::SyncPassword), Ok(Some(_)));
        if settings.encryption && existing && !password.force {
            return Err(
                "加密已开启，改密码会让已经上传的存档无法解密。确认要改请再确认一次".to_string(),
            );
        }

        // rclone owns this transformation; see `Runner::obscure`.
        let runner = self.sync_runner(&settings)?;
        let obscured = runner
            .obscure(&password.password)
            .await
            .map_err(|e| e.to_string())?;

        self.sync
            .keyring
            .set(SecretKey::SyncPassword, &password.password)
            .map_err(|e| e.to_string())?;
        self.sync
            .keyring
            .set(SecretKey::SyncPasswordObscured, &obscured)
            .map_err(|e| e.to_string())?;

        tracing::info!("sync password stored in the keyring (never on disk)");
        Ok(json!({
            "stored": true,
            // So the user can verify it is what they think it is, and read it
            // back later without kotori.
            "hint": crate::secrets::lookup_hint(SecretKey::SyncPassword),
        }))
    }

    /// Check credentials, bucket and write access.
    pub(super) async fn rpc_sync_test(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let remote = runner.check().await.map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "remote": remote }))
    }

    /// Upload now: one game, or every game that has save locations.
    pub(super) async fn rpc_sync_now(&self, game_id: Option<&str>) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;

        let ids: Vec<String> = match game_id {
            Some(id) => vec![id.to_string()],
            None => self
                .config
                .read()
                .await
                .games
                .iter()
                .filter(|(_, game)| !game.save_paths.is_empty())
                .map(|(id, _)| id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return Err("还没有任何游戏配置了存档位置".to_string());
        }

        let mut outcomes = Vec::with_capacity(ids.len());
        for id in ids {
            let (name, targets) = match self.sync_targets(&id).await {
                Ok(pair) => pair,
                Err(error) => {
                    outcomes.push(GameOutcome::failed(&id, &id, error));
                    continue;
                }
            };
            let outcome = runner.upload(&id, &name, &targets).await;
            self.sync.remember(&id, "上传", &outcome);
            outcomes.push(outcome);
        }

        Ok(json!({
            "ok": outcomes.iter().all(|o| o.ok),
            "games": outcomes,
        }))
    }

    /// The snapshots the cloud holds for a game.
    pub(super) async fn rpc_sync_versions(&self, game_id: &str) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let versions = runner.versions(game_id).await.map_err(|e| e.to_string())?;
        Ok(json!({ "versions": versions }))
    }

    /// Put a game's saves back. Without `version`, the newest state wins.
    pub(super) async fn rpc_sync_restore(
        &self,
        game_id: &str,
        version: Option<&str>,
    ) -> Result<Value, String> {
        // A bad snapshot name is invalid input, not a sync failure: reject it
        // here, before anything runs, and as a JSON-RPC error.
        if let Some(version) = version
            && !sync::is_snapshot(version)
        {
            return Err(format!(
                "不是合法的快照名: {version}（形如 20260911T101500Z）"
            ));
        }

        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let (name, targets) = self.sync_targets(game_id).await?;

        let outcome = runner.restore(game_id, &name, &targets, version).await;
        self.sync.remember(game_id, "恢复", &outcome);
        Ok(json!({ "ok": outcome.ok, "game": outcome }))
    }

    /// Pull the newest cloud state before a game starts.
    ///
    /// Returns `None` when there is nothing to do (sync off, no save paths, no
    /// rclone). Any failure is reported in the returned object and **never**
    /// stops the launch: the user asked to play a game.
    pub(super) async fn sync_pull_before_launch(&self, game_id: &str) -> Option<Value> {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return None;
            }
            let game = config.games.get(game_id)?;
            if game.save_paths.is_empty() {
                return None;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };

        let outcome =
            match tokio::time::timeout(PULL_TIMEOUT, runner.pull(game_id, &name, &targets)).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    tracing::warn!("{game_id}: 启动前拉取超时（{PULL_TIMEOUT:?}），直接启动游戏");
                    return Some(json!({
                        "ok": false,
                        "error": format!("拉取超过 {} 秒，已跳过", PULL_TIMEOUT.as_secs()),
                    }));
                }
            };

        self.sync.remember(game_id, "取回", &outcome);
        if !outcome.ok {
            tracing::warn!("{game_id}: 启动前拉取失败: {:?}", outcome.error);
        }
        Some(serde_json::to_value(&outcome).unwrap_or(Value::Null))
    }

    /// Upload after a game exits.
    ///
    /// Runs detached from the session watcher: an upload must never hold up the
    /// engine, and the daemon may be asked to shut down while it runs.
    pub(super) async fn sync_after_game_exit(&self, game_id: &str) {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::debug!("{game_id}: 跳过退出后上传: {error}");
                return;
            }
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => {
                tracing::warn!("{game_id}: 退出后上传失败: {error}");
                return;
            }
        };

        // Let wineserver finish flushing whatever the game just wrote.
        tokio::time::sleep(SETTLE_DELAY).await;

        let outcome = runner.upload(game_id, &name, &targets).await;
        self.sync.remember(game_id, "上传", &outcome);
        if outcome.ok {
            tracing::info!("{game_id}: 退出后已同步存档");
        } else {
            tracing::warn!("{game_id}: 退出后同步失败: {:?}", outcome.error);
        }
    }
}

/// The endpoint is optional and rarely needed; when it *is* set it has to be
/// something rclone can actually use, which is checked by [`sync::validate_endpoint`].
fn clean_endpoint(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    sync::validate_endpoint(trimmed).map_err(|e| e.to_string())?;
    Ok(trimmed.to_string())
}

fn clean_bucket(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.contains('/') || trimmed.contains(char::is_whitespace) {
        return Err(format!("bucket 名里不能有斜杠或空格（当前 {trimmed}）"));
    }
    Ok(trimmed.to_string())
}

/// The prefix is a folder inside the bucket that kotori owns. Keep it relative
/// and free of `..` so it can never be pointed at something else.
fn clean_prefix(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err("prefix 不能为空（它是 kotori 在 bucket 里独占的目录）".to_string());
    }
    if trimmed.contains("..") {
        return Err("prefix 里不能有 ..".to_string());
    }
    if trimmed.contains('\\') || trimmed.contains(':') {
        return Err("prefix 用 / 分隔，不要用 \\ 或 :".to_string());
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, GameConfig, SavePath, ScaleProfile};
    use crate::secrets::testing::FakeTool;
    use std::path::PathBuf;

    /// Send a raw JSON-RPC request through the real dispatcher.
    async fn call(daemon: &Daemon, method: &str, params: &str) -> Value {
        let request = if params.is_empty() {
            format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#)
        } else {
            format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#)
        };
        let reply = daemon.handle_request(&request).await;
        serde_json::from_str(&reply.body)
            .unwrap_or_else(|e| panic!("bad reply {}: {e}", reply.body))
    }

    fn daemon(keyring: Keyring) -> Daemon {
        let mut config = Config::default();
        config.sync.enabled = true;
        config.sync.endpoint = String::new();
        config.sync.bucket = "bkt".to_string();
        config.games.insert(
            "demo".into(),
            GameConfig {
                name: "demo".into(),
                game_dir: PathBuf::from("/games/demo"),
                exe_path: PathBuf::from("/games/demo/game.exe"),
                launch_args: Vec::new(),
                save_paths: vec![SavePath::inferred("savedata")],
                wine_prefix: None,
                watch_only: false,
                process_name: None,
                scale_profile: ScaleProfile::default_for((1920, 1080)),
                created_at: chrono::Utc::now(),
            },
        );
        // Tests own a throw-away config file: the daemon persists every settings
        // change, and the machine-wide config is not theirs to touch.
        let dir = std::env::temp_dir().join(format!(
            "kotori-sync-rpc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Daemon::with_keyring(config, keyring).with_config_path(dir.join("config.toml"))
    }

    #[tokio::test]
    async fn status_never_returns_a_secret_value() {
        let fake = FakeTool::new("status-secrets");
        let keyring = fake.keyring();
        keyring.set(SecretKey::B2KeyId, "keyid123").unwrap();
        keyring.set(SecretKey::B2AppKey, "appkey456").unwrap();

        let daemon = daemon(keyring);
        let value = call(&daemon, "sync.status", "").await;
        let body = value.to_string();
        let result = &value["result"];
        assert_eq!(result["enabled"], true);
        assert_eq!(result["settings"]["bucket"], "bkt");
        assert_eq!(result["remote"], "kotori:bkt/kotori");
        // Names only: the UI has no business holding the key.
        assert_eq!(result["secrets"][0], "b2-key-id");
        assert!(!body.contains("keyid123"), "{body}");
        assert!(!body.contains("appkey456"), "{body}");
        // And the user can always read the password back themselves.
        assert!(
            result["password_hint"]
                .as_str()
                .unwrap()
                .contains("secret-tool")
        );
        assert_eq!(result["games"][0]["locations"], 1);
    }

    #[tokio::test]
    async fn status_explains_what_is_missing_instead_of_failing() {
        let fake = FakeTool::new("status-missing");
        let value = call(&daemon(fake.keyring()), "sync.status", "").await;
        assert_eq!(value["result"]["ready"], false);
        assert!(
            value["result"]["problem"]
                .as_str()
                .unwrap()
                .contains("B2 凭据"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn settings_are_validated_before_they_are_stored() {
        let fake = FakeTool::new("settings");
        let daemon = daemon(fake.keyring());

        // An endpoint the backend cannot use, and paths that could escape the
        // prefix or the bucket.
        for (body, needle) in [
            // The B2 console shows the S3 endpoint first, and it is the wrong
            // one for this backend; say so instead of failing later with a 404.
            (
                r#"{"endpoint":"s3.us-west-004.backblazeb2.com"}"#,
                "S3 兼容接口",
            ),
            // rclone does not add a scheme, so a bare host cannot work.
            (r#"{"endpoint":"api001.backblazeb2.com"}"#, "https://"),
            (r#"{"bucket":"my/bucket"}"#, "bucket"),
            (r#"{"prefix":"../other"}"#, ".."),
            (r#"{"prefix":"  "}"#, "prefix"),
            (r#"{"keep_versions":100000}"#, "最多"),
            // Enabling with nowhere to sync to is refused.
            (r#"{"enabled":true,"bucket":""}"#, "bucket"),
        ] {
            let value = call(&daemon, "sync.set_settings", body).await;
            assert!(
                value["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(needle)),
                "expected {needle:?} in {value}"
            );
        }

        // Turning sync off and clearing the fields together is fine: an
        // unfinished setup is only a problem while sync is on.
        let value = call(
            &daemon,
            "sync.set_settings",
            r#"{"enabled":false,"endpoint":""}"#,
        )
        .await;
        assert_eq!(value["result"]["settings"]["enabled"], false);
        assert_eq!(value["result"]["settings"]["endpoint"], "");
        let value = call(&daemon, "sync.set_settings", r#"{"enabled":true}"#).await;
        assert_eq!(value["result"]["settings"]["enabled"], true);

        // A good patch is stored, and persisted.
        let value = call(
            &daemon,
            "sync.set_settings",
            r#"{"bucket":"new-bucket","prefix":"/saves/kotori/"}"#,
        )
        .await;
        assert_eq!(value["result"]["settings"]["bucket"], "new-bucket");
        assert_eq!(
            value["result"]["settings"]["prefix"], "saves/kotori",
            "the prefix is normalised, not rejected"
        );
    }

    #[tokio::test]
    async fn changing_encryption_needs_a_deliberate_confirmation() {
        let fake = FakeTool::new("encryption");
        let keyring = fake.keyring();
        keyring.set(SecretKey::SyncPassword, "hunter2").unwrap();
        let daemon = daemon(keyring);
        let patch = |body: &'static str| {
            let daemon = &daemon;
            async move { call(daemon, "sync.set_settings", body).await }
        };

        // Turning it on without acknowledging makes existing plain data
        // unreadable, so it is refused.
        let value = patch(r#"{"encryption":true}"#).await;
        assert!(
            value["error"]["message"].as_str().unwrap().contains("明文"),
            "{value}"
        );

        // With the confirmation it goes through.
        let value = patch(r#"{"encryption":true,"force":true}"#).await;
        assert_eq!(value["result"]["settings"]["encryption"], true);

        // Turning it off has the mirror problem.
        let value = patch(r#"{"encryption":false}"#).await;
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("无法解密"),
            "{value}"
        );
        let value = patch(r#"{"encryption":false,"force":true}"#).await;
        assert_eq!(value["result"]["settings"]["encryption"], false);
    }

    #[tokio::test]
    async fn enabling_encryption_requires_a_password_first() {
        let fake = FakeTool::new("encryption-nopass");
        let daemon = daemon(fake.keyring());
        let value = call(
            &daemon,
            "sync.set_settings",
            r#"{"encryption":true,"force":true}"#,
        )
        .await;
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("同步密码"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn credentials_go_into_the_keyring_and_can_be_cleared() {
        let fake = FakeTool::new("credentials");
        let keyring = fake.keyring();
        let daemon = daemon(keyring.clone());
        let credentials = |body: &'static str| {
            let daemon = &daemon;
            async move { call(daemon, "sync.set_credentials", body).await }
        };

        // Half a credential is refused: it can never work.
        let value = credentials(r#"{"key_id":"id","app_key":"  "}"#).await;
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("一起填"),
            "{value}"
        );

        let value = credentials(r#"{"key_id":"keyid","app_key":"appkey"}"#).await;
        assert_eq!(value["result"]["stored"], true);
        assert_eq!(
            keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("keyid")
        );
        // Values are trimmed: copied credentials often carry whitespace.
        let value = credentials(r#"{"key_id":" keyid2 ","app_key":" appkey2 "}"#).await;
        assert_eq!(value["result"]["stored"], true);
        assert_eq!(
            keyring.get(SecretKey::B2AppKey).unwrap().as_deref(),
            Some("appkey2")
        );

        let value = credentials(r#"{"key_id":"","app_key":""}"#).await;
        assert_eq!(value["result"]["cleared"], true);
        assert!(keyring.present().is_empty());
    }

    #[tokio::test]
    async fn a_launch_with_sync_off_does_not_touch_the_network() {
        let fake = FakeTool::new("launch-off");
        let mut config = Config::default();
        config.sync.enabled = false;
        let daemon = Daemon::with_keyring(config, fake.keyring());
        assert!(
            daemon.sync_pull_before_launch("demo").await.is_none(),
            "nothing to do, and nothing to report"
        );
    }

    #[tokio::test]
    async fn a_launch_never_fails_because_sync_is_broken() {
        let fake = FakeTool::new("launch-broken");
        let daemon = daemon(fake.keyring());

        // Sync is on but nothing is set up and there is no rclone: the launch
        // must still get an answer it can act on.
        let report = daemon
            .sync_pull_before_launch("demo")
            .await
            .expect("a report");
        assert_eq!(report["ok"], false);
        assert!(report["error"].is_string(), "{report}");
    }
}

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
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Daemon;
use crate::config::SyncConfig;
use crate::secrets::{EncryptedFile, Keyring, SecretKey};
use crate::sync::{
    self, SaveTarget,
    runner::{GameOutcome, PULL_TIMEOUT, Runner, SETTLE_DELAY},
};

/// Ceiling for the sliding window. A typo here decides how much history is
/// deleted, so the range is deliberately small and explicit.
pub const MAX_KEEP_VERSIONS: u32 = 100;

/// 由加密文件路径推出同目录的明文文件路径。`KOTORI_SECRETS_FILE` 的语义就是
/// "两个凭据文件都放这个目录"(见 `config::paths`),测试里用它给临时目录配对。
#[cfg(test)]
fn plain_sibling(secrets_path: &std::path::Path) -> std::path::PathBuf {
    secrets_path
        .parent()
        .map(|dir| dir.join("credentials.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("credentials.json"))
}

/// Keyring handle plus the last result per game, for the settings page.
pub(super) struct SyncState {
    keyring: Mutex<Keyring>,
    /// Where the master-password file lives. Held here (instead of being
    /// re-resolved) so a test can point it at a throw-away directory.
    secrets_path: std::path::PathBuf,
    /// Where the **plaintext** credential file lives (the default store).
    plain_path: std::path::PathBuf,
    /// Set when the store we fell back to is a session-only one. Since
    /// 2026-09-13 the fallback is the plaintext file, so this is only ever set
    /// by a caller that built a memory store on purpose (tests); it is kept
    /// because the "no keyring yet, maybe later" case must stay recoverable
    /// without restarting the daemon.
    retry_backend: AtomicBool,
    records: Mutex<HashMap<String, SyncRecord>>,
}

impl SyncState {
    /// The best store this machine can offer (明文文件 → 密钥环 → 主密码文件,见
    /// `Keyring::open_default` 的顺序说明)。
    pub(super) fn system() -> Self {
        let path = crate::config::secrets_path();
        let plain = crate::config::plain_secrets_path();
        let (keyring, retry) = Keyring::open_default(&path, &plain);
        Self::new(keyring, path, plain, retry)
    }

    fn new(
        keyring: Keyring,
        secrets_path: std::path::PathBuf,
        plain_path: std::path::PathBuf,
        retry: bool,
    ) -> Self {
        Self {
            keyring: Mutex::new(keyring),
            secrets_path,
            plain_path,
            retry_backend: AtomicBool::new(retry),
            records: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn secrets_path(&self) -> &std::path::Path {
        &self.secrets_path
    }

    /// 明文凭据文件的路径(默认落点)。
    pub(super) fn plain_path(&self) -> &std::path::Path {
        &self.plain_path
    }

    /// Use one specific store, and never second-guess it (tests do this).
    #[cfg(test)]
    pub(super) fn with_keyring(keyring: Keyring) -> Self {
        Self::new(
            keyring,
            crate::config::secrets_path(),
            crate::config::plain_secrets_path(),
            false,
        )
    }

    /// A store plus chosen file paths: a test points them at a throw-away
    /// directory so the real `~/.config/kotori/` is never touched.
    #[cfg(test)]
    pub(super) fn with_keyring_at(keyring: Keyring, secrets_path: std::path::PathBuf) -> Self {
        let plain = plain_sibling(&secrets_path);
        Self::new(keyring, secrets_path, plain, false)
    }

    /// A daemon that finds an existing master-password file, as after a restart.
    #[cfg(test)]
    pub(super) fn from_master_file(secrets_path: std::path::PathBuf) -> Self {
        let keyring = Keyring::encrypted_file(&secrets_path);
        let plain = plain_sibling(&secrets_path);
        Self::new(keyring, secrets_path, plain, false)
    }

    /// Switch to a store we just created, and stop second-guessing the choice.
    pub(super) fn adopt(&self, keyring: Keyring) {
        if let Ok(mut current) = self.keyring.lock() {
            *current = keyring;
        }
        self.retry_backend.store(false, Ordering::Relaxed);
    }

    /// The best store available right now.
    ///
    /// Re-checks the platform keyring while we are on the session-only
    /// fallback, and hands over anything the user stored meanwhile.
    pub(super) fn keyring(&self) -> Keyring {
        let Ok(mut current) = self.keyring.lock() else {
            return Keyring::memory();
        };
        if !self.retry_backend.load(Ordering::Relaxed) {
            return current.clone();
        }

        let (fresh, still_missing) = Keyring::open_default(&self.secrets_path, &self.plain_path);
        if still_missing {
            // Keep the session store; its contents are still the user's.
            return current.clone();
        }

        // Hand the session's secrets over, unless the new store needs a
        // password first — a locked file cannot accept them yet, and dropping
        // them on the floor would be worse than leaving them in memory.
        let carried = current.snapshot();
        let locked = matches!(
            fresh.kind(),
            crate::secrets::StoreKind::EncryptedFile { locked: true, .. }
        );
        if !carried.is_empty() && !locked {
            tracing::info!("有可持久化的凭据后端了，迁移 {} 条临时凭据", carried.len());
            for (key, value) in carried {
                if let Err(error) = fresh.set(key, &value) {
                    tracing::warn!("迁移 {} 失败: {error}", key.account());
                }
            }
        }
        tracing::info!("凭据后端现在可用：{}", fresh.describe());
        *current = fresh.clone();
        self.retry_backend.store(false, Ordering::Relaxed);
        fresh
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
        Runner::new(settings.clone(), self.sync.keyring()).map_err(|e| e.to_string())
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
            .keyring()
            .present()
            .into_iter()
            .map(|key| key.account())
            .collect();

        // Only meaningful once sync is on; an unfinished setup is not an error
        // while the user is still typing.
        let problem = if settings.enabled {
            sync::validate(&settings)
                .err()
                .or_else(|| sync::validate_secrets(&settings, &self.sync.keyring()).err())
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
                "backend": self.sync.keyring().describe(),
                "ephemeral": self.sync.keyring().is_ephemeral(),
                // Which store, and whether it still needs a password. The UI
                // needs both to offer "unlock" instead of "enter credentials".
                "store": self.sync.keyring().kind(),
                "secrets_file": self.sync.secrets_path().display().to_string(),
                "min_master_password": crate::secrets::encrypted::MIN_MASTER_PASSWORD,
            },
            // Which entries exist — never what they contain.
            "secrets": secrets,
            "ready": settings.enabled && problem.is_none() && rclone.is_some(),
            "problem": problem,
            "remote": sync::remote_root(&settings),
            "password_hint": self.sync.keyring().lookup_hint(SecretKey::SyncPassword),
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
        let keyring = self.sync.keyring();
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
                .keyring()
                .clear(SecretKey::B2KeyId)
                .map_err(|e| e.to_string())?;
            self.sync
                .keyring()
                .clear(SecretKey::B2AppKey)
                .map_err(|e| e.to_string())?;
            return Ok(json!({ "cleared": true }));
        }

        if key_id.is_empty() || app_key.is_empty() {
            return Err("key id 和 application key 要一起填".to_string());
        }

        self.sync
            .keyring()
            .set(SecretKey::B2KeyId, key_id)
            .map_err(|e| e.to_string())?;
        self.sync
            .keyring()
            .set(SecretKey::B2AppKey, app_key)
            .map_err(|e| e.to_string())?;
        tracing::info!("B2 credentials stored in the keyring");
        Ok(json!({ "stored": true }))
    }

    /// Store (or clear) the sync password, in both the readable and rclone form.
    pub(super) async fn rpc_sync_set_password(&self, password: Password) -> Result<Value, String> {
        if password.password.is_empty() {
            self.sync
                .keyring()
                .clear(SecretKey::SyncPassword)
                .map_err(|e| e.to_string())?;
            self.sync
                .keyring()
                .clear(SecretKey::SyncPasswordObscured)
                .map_err(|e| e.to_string())?;
            return Ok(json!({ "cleared": true }));
        }

        let settings = self.config.read().await.sync.clone();
        let existing = matches!(
            self.sync.keyring().get(SecretKey::SyncPassword),
            Ok(Some(_))
        );
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
            .keyring()
            .set(SecretKey::SyncPassword, &password.password)
            .map_err(|e| e.to_string())?;
        self.sync
            .keyring()
            .set(SecretKey::SyncPasswordObscured, &obscured)
            .map_err(|e| e.to_string())?;

        tracing::info!("sync password stored (never in the config)");
        Ok(json!({
            "stored": true,
            // So the user can verify it is what they think it is, and read it
            // back later without kotori — in the way *this* store allows.
            "hint": self.sync.keyring().lookup_hint(SecretKey::SyncPassword),
        }))
    }

    /// Unlock the master-password file with the password the user just typed.
    pub(super) fn rpc_sync_unlock(&self, password: Password) -> Result<Value, String> {
        let keyring = self.sync.keyring();
        let Some(file) = keyring.encrypted_store() else {
            // 如实报现在用的是哪一级:说"存在系统密钥环"在只跑内存的机器上是假话。
            return Err(format!(
                "当前不需要解锁（凭据存在{}里）",
                keyring.describe()
            ));
        };
        file.unlock(&password.password).map_err(|e| e.to_string())?;
        tracing::info!("凭据文件已解锁");
        Ok(json!({ "unlocked": true, "store": keyring.kind() }))
    }

    /// Move whatever credentials we have into a master-password file.
    ///
    /// This is the escape hatch for machines with no OS keyring: without it,
    /// every restart would ask for the B2 keys again, which is exactly what
    /// makes unattended sync impossible on those systems.
    pub(super) fn rpc_sync_set_master_password(&self, password: Password) -> Result<Value, String> {
        let path = self.sync.secrets_path().to_path_buf();
        let existing = EncryptedFile::new(&path);
        if existing.exists() && !password.force {
            return Err(
                "已经有一个主密码凭据文件了；重设主密码会重新加密它（旧密码立即失效），确认请再点一次"
                    .to_string(),
            );
        }

        let current = self.sync.keyring();
        let entries: Vec<(SecretKey, String)> = SecretKey::ALL
            .into_iter()
            .filter_map(|key| match current.get(key) {
                Ok(Some(value)) => Some((key, value)),
                _ => None,
            })
            .collect();

        existing
            .create(&password.password, &entries)
            .map_err(|e| e.to_string())?;
        // Adopt the very handle we just sealed: a fresh one would be locked.
        self.sync.adopt(Keyring::from_encrypted(existing));

        tracing::info!(
            "凭据已存入主密码文件 {}（{} 条）",
            path.display(),
            entries.len()
        );
        Ok(json!({
            "stored": true,
            "path": path.display().to_string(),
            "count": entries.len(),
        }))
    }

    /// Delete the master-password file.
    ///
    /// The credentials in it go with it; on a machine with no keyring that
    /// means they are gone. The UI asks twice.
    pub(super) fn rpc_sync_clear_master_password(&self) -> Result<Value, String> {
        let path = self.sync.secrets_path().to_path_buf();
        let file = EncryptedFile::new(&path);
        if !file.exists() {
            return Err("没有主密码凭据文件".to_string());
        }
        file.remove().map_err(|e| e.to_string())?;

        let (fresh, _) = Keyring::open_default(&path, self.sync.plain_path());
        self.sync.adopt(fresh);
        tracing::warn!("主密码凭据文件已删除: {}", path.display());
        Ok(json!({ "removed": true }))
    }

    /// Forget the key until the password is entered again.
    pub(super) fn rpc_sync_lock(&self) -> Result<Value, String> {
        let keyring = self.sync.keyring();
        let Some(file) = keyring.encrypted_store() else {
            return Err("当前不是主密码凭据文件模式".to_string());
        };
        file.lock();
        Ok(json!({ "locked": true, "store": keyring.kind() }))
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

    /// The sync settings every test here starts from.
    fn daemon_config() -> Config {
        let mut config = Config::default();
        config.sync.enabled = true;
        config.sync.bucket = "bkt".to_string();
        config
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
                scale_profile: ScaleProfile::default_for(),
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
    async fn a_master_password_seals_the_credentials_and_survives_a_restart() {
        // The whole point of this backend: on a machine with no OS keyring the
        // B2 keys must not have to be retyped after every reboot, or unattended
        // sync is impossible there.
        let dir = std::env::temp_dir().join(format!(
            "kotori-master-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("secrets.json");

        // --- a machine with nothing but a session store ---------------------
        let daemon = Daemon::with_keyring_at(daemon_config(), Keyring::memory(), path.clone());
        call(
            &daemon,
            "sync.set_credentials",
            r#"{"key_id":"005keyid","app_key":"K005appkey"}"#,
        )
        .await;

        let value = call(
            &daemon,
            "sync.set_master_password",
            r#"{"password":"correct horse battery","force":true}"#,
        )
        .await;
        assert_eq!(value["result"]["stored"], true, "{value}");
        assert_eq!(value["result"]["count"], 2);
        assert!(path.is_file(), "凭据文件应当被创建");

        // The plaintext must not be anywhere in the file.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("K005appkey"), "{raw}");

        // And the daemon is now using that file, unlocked.
        let status = call(&daemon, "sync.status", "").await;
        assert_eq!(
            status["result"]["keyring"]["store"]["kind"],
            "encrypted-file"
        );
        assert_eq!(status["result"]["keyring"]["store"]["locked"], false);
        // ⚠ 不要断言 `ready`:它还要求 PATH 上有 rclone(`ready = enabled &&
        // problem.is_none() && rclone.is_some()`),而这条测试讲的是凭据本身 ——
        // 在没装 rclone 的机器上(CI 就是)它会红得毫无道理。凭据在不在,
        // 看 `secrets` 与 `ephemeral` 就够了。
        assert_eq!(status["result"]["secrets"].as_array().unwrap().len(), 2);
        assert_eq!(status["result"]["keyring"]["ephemeral"], false, "{status}");

        // --- the same machine after a restart ------------------------------
        let restarted = Daemon::with_master_file(daemon_config(), path.clone());
        let status = call(&restarted, "sync.status", "").await;
        assert_eq!(
            status["result"]["keyring"]["store"]["kind"],
            "encrypted-file"
        );
        assert_eq!(
            status["result"]["keyring"]["store"]["locked"], true,
            "重启后应当是锁定的"
        );
        assert!(
            status["result"]["problem"]
                .as_str()
                .unwrap()
                .contains("已锁定"),
            "锁定时要说「已锁定」，不能说「还没有凭据」: {status}"
        );
        // The credentials are still there — just not readable yet.
        assert!(status["result"]["secrets"].as_array().unwrap().is_empty());

        // A wrong password changes nothing.
        let value = call(&restarted, "sync.unlock", r#"{"password":"nope"}"#).await;
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("主密码"),
            "{value}"
        );
        assert_eq!(
            call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
            true
        );

        // The right one opens it, and the credentials come back.
        let value = call(
            &restarted,
            "sync.unlock",
            r#"{"password":"correct horse battery"}"#,
        )
        .await;
        assert_eq!(value["result"]["unlocked"], true, "{value}");
        let status = call(&restarted, "sync.status", "").await;
        assert_eq!(status["result"]["keyring"]["store"]["locked"], false);
        assert_eq!(status["result"]["secrets"].as_array().unwrap().len(), 2);
        // 同上:这里证明"解锁之后凭据回来了",不是"这台机器能同步"。
        assert_eq!(status["result"]["keyring"]["ephemeral"], false, "{status}");

        // Locking again hides them without destroying anything.
        let value = call(&restarted, "sync.lock", "").await;
        assert_eq!(value["result"]["locked"], true, "{value}");
        assert_eq!(
            call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
            true
        );
        // ...and unlocking still works, so nothing was thrown away.
        call(
            &restarted,
            "sync.unlock",
            r#"{"password":"correct horse battery"}"#,
        )
        .await;
        assert_eq!(
            call(&restarted, "sync.status", "").await["result"]["secrets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 忘了主密码时的出路:删凭据文件**不需要先解锁**(GUI 的「删除凭据文件」就走这条,
    /// 所以它必须一直可用 —— 否则锁着的文件就成了删不掉的垃圾)。
    #[tokio::test]
    async fn the_master_file_can_be_deleted_even_while_it_is_locked() {
        let dir = std::env::temp_dir().join(format!(
            "kotori-master-clear-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("secrets.json");

        let daemon = Daemon::with_keyring_at(daemon_config(), Keyring::memory(), path.clone());
        call(
            &daemon,
            "sync.set_credentials",
            r#"{"key_id":"005keyid","app_key":"K005appkey"}"#,
        )
        .await;
        call(
            &daemon,
            "sync.set_master_password",
            r#"{"password":"correct horse battery","force":true}"#,
        )
        .await;
        assert!(path.is_file(), "凭据文件应当被创建");

        // 重启后文件是锁着的 —— 正是"忘了主密码"的那种状态。
        let restarted = Daemon::with_master_file(daemon_config(), path.clone());
        assert_eq!(
            call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
            true
        );

        let value = call(&restarted, "sync.clear_master_password", "").await;
        assert_eq!(value["result"]["removed"], true, "{value}");
        assert!(!path.is_file(), "文件应当被删掉");

        // 没有文件后端了,而且如实说现在用哪一级 —— 但不断言是哪一级:
        // 这台机器上有没有真密钥环不是这个测试能决定的。
        let status = call(&restarted, "sync.status", "").await;
        assert_ne!(
            status["result"]["keyring"]["store"]["kind"], "encrypted-file",
            "文件都没了,不该还说自己在用文件后端: {status}"
        );

        // 再删一次要明说"没有文件",而不是假装成功。
        let value = call(&restarted, "sync.clear_master_password", "").await;
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("没有主密码凭据文件"),
            "{value}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_short_master_password_is_refused_with_a_reason() {
        let dir = std::env::temp_dir().join(format!(
            "kotori-master-short-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let daemon =
            Daemon::with_keyring_at(daemon_config(), Keyring::memory(), dir.join("secrets.json"));
        call(
            &daemon,
            "sync.set_credentials",
            r#"{"key_id":"k","app_key":"s"}"#,
        )
        .await;

        let value = call(
            &daemon,
            "sync.set_master_password",
            r#"{"password":"short","force":true}"#,
        )
        .await;
        assert!(
            value["error"]["message"].as_str().unwrap().contains("至少"),
            "{value}"
        );
        assert!(!dir.join("secrets.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_explicitly_given_store_is_never_replaced_behind_the_caller() {
        // Only the *fallback* is retried. A store handed in deliberately (tests,
        // and any future backend) must stay in use, otherwise every test that
        // seeds a fake keyring would silently start reading the real one.
        let keyring = Keyring::memory();
        keyring.set(SecretKey::B2KeyId, "seeded").unwrap();
        let state = SyncState::with_keyring(keyring);

        assert!(!state.retry_backend.load(Ordering::Relaxed));
        for _ in 0..3 {
            let current = state.keyring();
            assert_eq!(
                current.get(SecretKey::B2KeyId).unwrap().as_deref(),
                Some("seeded")
            );
            assert!(current.is_ephemeral());
        }
    }

    #[test]
    fn a_session_store_can_hand_its_secrets_over() {
        // What makes the fallback survivable: credentials typed while no
        // keyring was running are carried into the real one once it appears.
        let keyring = Keyring::memory();
        assert!(keyring.snapshot().is_empty());

        keyring.set(SecretKey::B2KeyId, "id").unwrap();
        keyring.set(SecretKey::B2AppKey, "key").unwrap();
        let mut carried = keyring.snapshot();
        carried.sort_by_key(|(key, _)| key.account());

        assert_eq!(carried.len(), 2);
        assert_eq!(carried[0].0, SecretKey::B2AppKey);
        assert_eq!(carried[0].1, "key");
        assert_eq!(carried[1].0, SecretKey::B2KeyId);

        // A real keyring has nothing to hand over.
        assert!(
            Keyring::with_tool("/usr/bin/secret-tool")
                .snapshot()
                .is_empty()
        );
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

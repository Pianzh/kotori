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
use crate::config::{SyncConfig, SyncEngine};
use crate::secrets::{Keyring, SecretKey};
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
    records: Mutex<HashMap<String, SyncRecord>>,
}

impl SyncState {
    /// The best store this machine can offer (明文文件 → 密钥环 → 主密码文件,见
    /// `Keyring::open_default` 的顺序说明)。
    pub(super) fn system() -> Self {
        let path = crate::config::secrets_path();
        let plain = crate::config::plain_secrets_path();
        let keyring = Keyring::open_default(&path, &plain);
        Self::new(keyring, path, plain)
    }

    fn new(
        keyring: Keyring,
        secrets_path: std::path::PathBuf,
        plain_path: std::path::PathBuf,
    ) -> Self {
        Self {
            keyring: Mutex::new(keyring),
            secrets_path,
            plain_path,
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
        )
    }

    /// A store plus chosen file paths: a test points them at a throw-away
    /// directory so the real `~/.config/kotori/` is never touched.
    #[cfg(test)]
    pub(super) fn with_keyring_at(keyring: Keyring, secrets_path: std::path::PathBuf) -> Self {
        let plain = plain_sibling(&secrets_path);
        Self::new(keyring, secrets_path, plain)
    }

    /// A daemon that finds an existing master-password file, as after a restart.
    #[cfg(test)]
    pub(super) fn from_master_file(secrets_path: std::path::PathBuf) -> Self {
        let keyring = Keyring::encrypted_file(&secrets_path);
        let plain = plain_sibling(&secrets_path);
        Self::new(keyring, secrets_path, plain)
    }

    /// Switch to a store we just created, and stop second-guessing the choice.
    pub(super) fn adopt(&self, keyring: Keyring) {
        if let Ok(mut current) = self.keyring.lock() {
            *current = keyring;
        }
    }

    /// The store in use.
    ///
    /// ⚠ 这里从前还有一段"落到内存后端了就重新探一遍密钥环、起来了就把凭据接管过去"的
    /// 逻辑 —— 那段路**到不了**:明文凭据文件永远能当兜底,所以生产路径上不存在
    /// session-only 这一级(ADR-014 写的四级里只有三级可达)。2026-09-13 删掉,连同
    /// 它那个恒为 `false` 的 `retry_backend` 开关;密钥环晚一步起来的情况,重启
    /// daemon 就会在启动时走"把明文搬进密钥环"那一条。
    pub(super) fn keyring(&self) -> Keyring {
        let Ok(current) = self.keyring.lock() else {
            return Keyring::memory();
        };
        current.clone()
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
///
/// ⚠ `deny_unknown_fields` 是**故意**的（照 `GamePatch` 的规矩来）：没有它的时候，
/// 调用方多发一个键（比如界面新加了一项、而这里忘了跟上）会被**静默丢掉**，却仍然
/// 拿到 `success: true` —— 调用方以为改了配置，其实一个字节都没动。写这两个"程序
/// 位置"时就真踩了一次：界面把 `kopia_binary` 发出去了，这里没这个字段，用户填的
/// 路径消失了而且不报错（e2e 抓到的）。现在这种包法直接报"参数无效"。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsPatch {
    enabled: Option<bool>,
    engine: Option<SyncEngine>,
    endpoint: Option<String>,
    bucket: Option<String>,
    prefix: Option<String>,
    keep_versions: Option<u32>,
    /// 两个引擎的可执行文件在哪（空串 = 清掉，回到"自己找"）。
    rclone_binary: Option<String>,
    kopia_binary: Option<String>,
}

/// A password a client sent, plus "yes, I mean it" when the change cannot be
/// undone. Used by the master-password file (the sync password it used to serve
/// went away with the crypt layer).
#[derive(Debug, Deserialize)]
pub(super) struct Password {
    #[serde(default)]
    pub(super) password: String,
    #[serde(default)]
    pub(super) force: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct Credentials {
    #[serde(default)]
    key_id: String,
    #[serde(default)]
    app_key: String,
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
        let rclone =
            sync::find_rclone(&settings.rclone_binary).map(|p| p.to_string_lossy().to_string());
        let kopia =
            sync::find_kopia(&settings.kopia_binary).map(|p| p.to_string_lossy().to_string());
        // 当前生效的引擎要哪个二进制。选中的那个没装 = 没准备好；另一个没有
        // 不影响什么（用户可以只装一个）。
        let engine_binary = match settings.engine {
            SyncEngine::Rclone => rclone.clone(),
            SyncEngine::Kopia => kopia.clone(),
        };
        let secrets: Vec<&str> = self
            .sync
            .keyring()
            .present()
            .into_iter()
            .map(|key| key.account())
            .collect();

        // Only meaningful once sync is on; an unfinished setup is not an error
        // while the user is still typing.
        // 只有"开着同步"时才谈"为什么跑不起来"：还在填的过程中不算错。
        // 判据本身在 `actions::readiness_problem`（它只依赖参数，所以能单独测）。
        let problem = if settings.enabled {
            actions::readiness_problem(&settings, &self.sync.keyring(), engine_binary.as_deref())
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
            "engine": settings.engine,
            "engine_label": settings.engine.label(),
            "rclone": rclone,
            "kopia": kopia,
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
            "ready": settings.enabled && problem.is_none() && engine_binary.is_some(),
            "problem": problem,
            // rclone 那条路的远端；kopia 整个仓库落在 `kopia_prefix` 下。
            "remote": sync::remote_root(&settings),
            "kopia_prefix": sync::engine::repo_prefix(&settings),
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
        self.mutate_config(|config| {
            let mut candidate = config.sync.clone();
            let previous_engine = candidate.engine;

            if let Some(enabled) = patch.enabled {
                candidate.enabled = enabled;
            }
            if let Some(engine) = patch.engine {
                candidate.engine = engine;
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
            // 两个"程序位置"：界面填什么就是什么（目录或完整路径都行，怎么解释见
            // `sync::executables`）。空串 = 清掉，回到自己找。
            if let Some(value) = &patch.rclone_binary {
                candidate.rclone_binary = value.trim().to_string();
            }
            if let Some(value) = &patch.kopia_binary {
                candidate.kopia_binary = value.trim().to_string();
            }

            if candidate.enabled {
                sync::validate(&candidate).map_err(|e| e.to_string())?;
            }

            config.sync = candidate.clone();
            tracing::info!(
                "sync settings updated (enabled={}, engine={:?}, keep_versions={})",
                candidate.enabled,
                candidate.engine,
                candidate.keep_versions
            );
            // 换引擎要专门告诉 UI：两个引擎在桶里各写各的区域，换了之后对面那些
            // 版本**不会**被读出来（数据还在桶里，只是看不见），而这件事不会报错。
            let engine_changed = candidate.engine != previous_engine;
            Ok(json!({
                "settings": candidate,
                "engine_changed": engine_changed,
            }))
        })
        .await
    }

    /// 设置（或清除）kopia 仓库密码。
    ///
    /// 留空 = 清除 = 回到默认的 `kotori`。默认密码意味着"任何拿到桶的人都能解开"，
    /// 所以这条 RPC 的回话里带着 [`DEFAULT_PASSWORD_USED`] 让 UI 能如实提醒。
    pub(super) fn rpc_sync_set_kopia_password(&self, password: Password) -> Result<Value, String> {
        let value = password.password.trim();
        if value.is_empty() {
            self.sync
                .keyring()
                .clear(SecretKey::KopiaPassword)
                .map_err(|e| e.to_string())?;
            tracing::info!("kopia repository password cleared (back to the default)");
            return Ok(json!({ "cleared": true, "using_default": true }));
        }
        self.sync
            .keyring()
            .set(SecretKey::KopiaPassword, value)
            .map_err(|e| e.to_string())?;
        tracing::info!("kopia repository password stored in the keyring");
        Ok(json!({ "stored": true, "using_default": false }))
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

mod actions;
mod credentials;
#[cfg(test)]
mod tests;

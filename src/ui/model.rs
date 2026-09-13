//! Plain data shared by the UI. The message/page enums live in
//! [`super::message`]; everything a daemon answer is turned into lives here.

use super::*;

/// Wine prefix situation on this machine (settings page).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WineStatus {
    pub configured: Option<String>,
    pub default_prefix: String,
    pub environment: Option<String>,
    pub detected: Vec<String>,
}

/// 全局快捷键现在注册成什么样了,来自 `daemon.status.hotkeys`。
///
/// ⚠ 后端目前只报「注册得怎么样」,不报「11 个动作各绑了什么键」——
/// `unbound` 是"桌面授权了但没给键"的那些动作,是"按了没反应"的唯一解释。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeyStatus {
    pub requested: bool,
    pub ready: bool,
    pub error: Option<String>,
    pub unbound: Vec<String>,
    /// 这个桌面去哪里绑键(KDE/GNOME 说法不同,由 `hotkeys::assign_hint` 定)。
    pub assign_hint: String,
}

/// Maximum number of automatic reconnect attempts before giving up (a manual
/// "重连" always works, and resets the counter).
pub(super) const MAX_AUTO_RETRIES: u32 = 5;

/// How often the UI polls the daemon for live sessions.
pub(super) const STATUS_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// The three save-location kinds, as shown in the editor.
pub(super) const SAVE_PATH_KINDS: [&str; 3] = ["windows", "relative", "absolute"];

/// One save location in the editor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavePathDraft {
    /// `windows` | `relative` | `absolute`.
    pub kind: String,
    pub path: String,
    /// Comma-separated glob patterns.
    pub exclude: String,
}

/// One game's sync situation, as reported by `sync.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncGameRow {
    pub id: String,
    pub name: String,
    pub locations: u64,
    /// Set when a save location cannot be resolved right now (unplugged disk,
    /// removed prefix) — better to say so than to fail at sync time.
    pub problem: Option<String>,
    /// Human-readable "when and how it went" for the last sync.
    pub last: Option<String>,
}

impl SyncGameRow {
    /// One line describing the last sync of this game.
    pub(super) fn last_label(&self) -> String {
        match &self.last {
            Some(last) => last.clone(),
            None => "还没同步过".to_string(),
        }
    }
}

/// Cloud-sync state for the settings page. Never carries a secret *value*.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncStatus {
    /// The non-secret settings, as stored (`[sync]` in the config).
    pub settings: Value,
    pub remote: String,
    pub rclone: Option<String>,
    pub keyring: String,
    pub ephemeral: bool,
    /// `system` | `encrypted-file` | `session-only`.
    pub store_kind: String,
    /// Only meaningful for `encrypted-file`.
    pub store_locked: bool,
    pub store_path: String,
    pub min_master_password: usize,
    pub secrets: Vec<String>,
    pub ready: bool,
    pub problem: Option<String>,
    pub password_hint: String,
    pub games: Vec<SyncGameRow>,
}

impl SyncStatus {
    /// Whether one of our credential slots is filled. `sync.status` reports
    /// account names only — never a value.
    pub(super) fn has_secret(&self, account: &str) -> bool {
        self.secrets.iter().any(|a| a == account)
    }
}

/// The editable half of the sync settings.
#[derive(Debug, Clone, Default)]
pub(super) struct SyncForm {
    pub(super) loaded: bool,
    /// Set as soon as the user edits a *settings* field. `sync.status` replies
    /// can land seconds after the request (the daemon probes the keyring on the
    /// way), so a reply that was already in flight must never overwrite what
    /// the user is in the middle of typing. Cleared once a save succeeds.
    pub(super) settings_dirty: bool,
    pub(super) enabled: bool,
    pub(super) endpoint: String,
    pub(super) bucket: String,
    pub(super) prefix: String,
    pub(super) keep_versions: String,
    pub(super) encryption: bool,
    pub(super) key_id: String,
    pub(super) app_key: String,
    pub(super) password: String,
    pub(super) password_again: String,
    /// Master password for the credential file (unlock, or set one up).
    pub(super) master_password: String,
    /// An encryption change needs one more click: it decides whether existing
    /// data in the bucket can still be read.
    pub(super) confirm_encryption: Option<bool>,
    pub(super) msg: Option<String>,
    pub(super) busy: bool,
}

impl SyncForm {
    /// Fill the form from what the daemon reports. Secrets are never echoed, so
    /// their inputs are left alone here: they are only cleared when a save
    /// actually consumed them (`SyncCredentialsSaved` / `SyncPasswordSaved`).
    ///
    /// Everything is skipped while `settings_dirty` is set — see the field.
    pub(super) fn apply(&mut self, status: &SyncStatus, settings: &Value) {
        self.loaded = true;
        self.confirm_encryption = None;
        let _ = status;
        if self.settings_dirty {
            return;
        }
        self.enabled = settings["enabled"].as_bool().unwrap_or(false);
        self.endpoint = str_field(settings, "endpoint");
        self.bucket = str_field(settings, "bucket");
        self.prefix = str_field(settings, "prefix");
        self.keep_versions = settings["keep_versions"].as_u64().unwrap_or(0).to_string();
        self.encryption = settings["encryption"].as_bool().unwrap_or(false);
    }

    /// The patch sent to `sync.set_settings`.
    pub(super) fn patch(&self, force: bool) -> Value {
        let keep = self.keep_versions.trim().parse::<u32>().unwrap_or(0);
        serde_json::json!({
            "enabled": self.enabled,
            "endpoint": self.endpoint.trim(),
            "bucket": self.bucket.trim(),
            "prefix": self.prefix.trim(),
            "keep_versions": keep,
            "encryption": self.encryption,
            "force": force,
        })
    }
}

/// A live session, as reported by `daemon.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub watch_only: bool,
}

#[derive(Debug, Clone)]
pub struct UiGame {
    pub id: String,
    pub name: String,
    pub game_dir: String,
    pub exe: String,
    pub save_paths: Vec<SavePathDraft>,
    /// Watch-only games are started by the user; kotori follows the process.
    pub watch_only: bool,
    pub process_name: String,
    /// Profile name as stored, so saving never silently renames it.
    pub profile_name: String,
    pub algo: String,
    pub sharpness: u32,
    pub internal: (u32, u32),
    pub output: (u32, u32),
    /// Scaling ratio as stored; `None` means the profile still drives the output
    /// size from `output` alone.
    pub scale_ratio: Option<f32>,
    /// Whether the window size may drive the output size (i.e. dragging the
    /// window rescales live).
    pub follow_window: bool,
    pub fullscreen: bool,
    pub framerate: Option<u32>,
}

/// Editable copy of a game's scale profile.
#[derive(Debug, Clone)]
pub(super) struct Draft {
    pub(super) game_id: String,
    pub(super) profile_name: String,
    /// Editable game root and exe path, plus their stored values so unchanged
    /// fields are not re-sent (the daemon rejects a path that does not exist,
    /// e.g. when the game lives on a drive that is not mounted right now).
    pub(super) game_dir: String,
    pub(super) game_dir_original: String,
    pub(super) exe: String,
    pub(super) exe_original: String,
    pub(super) save_paths: Vec<SavePathDraft>,
    pub(super) save_paths_original: Vec<SavePathDraft>,
    pub(super) algo: String,
    pub(super) sharpness: u32,
    pub(super) internal_w: String,
    pub(super) internal_h: String,
    pub(super) output_w: String,
    pub(super) output_h: String,
    /// Kept as text so a half-typed ratio survives an edit. Empty means "no
    /// ratio". The widget for it arrives with the scale-section rework; until
    /// then this only carries the stored value through an open + save.
    pub(super) scale_ratio: String,
    pub(super) follow_window: bool,
    pub(super) fullscreen: bool,
    pub(super) framerate: String,
}

impl Draft {
    /// Seed the form from the *stored* profile. Anything else means a plain
    /// "open + save" silently rewrites the user's settings.
    pub(super) fn from_game(game: &UiGame) -> Self {
        Self {
            game_id: game.id.clone(),
            profile_name: game.profile_name.clone(),
            game_dir: game.game_dir.clone(),
            game_dir_original: game.game_dir.clone(),
            exe: game.exe.clone(),
            exe_original: game.exe.clone(),
            save_paths: game.save_paths.clone(),
            save_paths_original: game.save_paths.clone(),
            algo: if ScaleAlgorithm::ALL.contains(&game.algo.as_str()) {
                game.algo.clone()
            } else {
                ScaleAlgorithm::Fsr {
                    sharpness: game.sharpness,
                }
                .label()
                .to_string()
            },
            sharpness: game.sharpness,
            internal_w: game.internal.0.to_string(),
            internal_h: game.internal.1.to_string(),
            output_w: game.output.0.to_string(),
            output_h: game.output.1.to_string(),
            scale_ratio: game.scale_ratio.map(|r| r.to_string()).unwrap_or_default(),
            follow_window: game.follow_window,
            fullscreen: game.fullscreen,
            framerate: game.framerate.map(|f| f.to_string()).unwrap_or_default(),
        }
    }

    /// Has the user changed the exe path?
    pub(super) fn exe_changed(&self) -> bool {
        self.exe.trim() != self.exe_original
    }

    /// Has the user changed the game root?
    pub(super) fn game_dir_changed(&self) -> bool {
        self.game_dir.trim() != self.game_dir_original
    }

    /// Has the user changed the save locations?
    pub(super) fn save_paths_changed(&self) -> bool {
        self.save_paths != self.save_paths_original
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

    #[test]
    fn draft_seeds_exe_and_detects_changes() {
        let game = ui_game();
        let mut draft = Draft::from_game(&game);
        assert_eq!(draft.exe, game.exe);
        assert!(
            !draft.exe_changed(),
            "opening a game must not count as an edit"
        );

        draft.exe = "/games/demo/other.exe".into();
        assert!(draft.exe_changed());

        // Whitespace-only differences are not an edit either.
        let mut padded = Draft::from_game(&game);
        padded.exe = format!("  {}  ", game.exe);
        assert!(!padded.exe_changed());
    }

    #[test]
    fn the_sync_form_seeds_from_settings_and_never_from_secrets() {
        let payload = sync_payload();
        let mut form = SyncForm::default();
        form.apply(&sync_status_fixture(), &payload["settings"]);

        assert!(form.loaded);
        assert!(form.enabled);
        assert_eq!(form.endpoint, "");
        assert_eq!(form.bucket, "kotori-saves");
        assert_eq!(form.prefix, "kotori");
        assert_eq!(form.keep_versions, "0");
        assert!(!form.encryption);

        // The daemon reports *which* secrets exist, never their values, so the
        // inputs must start empty even though three are stored.
        assert!(form.key_id.is_empty());
        assert!(form.app_key.is_empty());
        assert!(form.password.is_empty());
        assert!(form.password_again.is_empty());

        // The patch mirrors the form, trimmed.
        form.bucket = "  spaced  ".into();
        form.confirm_encryption = Some(true);
        let patch = form.patch(true);
        assert_eq!(patch["bucket"], "spaced");
        assert_eq!(patch["force"], true);
        assert_eq!(patch["enabled"], true);
        assert!(
            patch.get("key_id").is_none() && patch.get("password").is_none(),
            "settings patches must carry no secrets: {patch}"
        );
    }
}

//! The application state itself, plus the parts of `impl App` that are neither
//! the message loop (`super::update`) nor the view tree (`super::view`).

use super::*;

pub struct App {
    pub(super) tab: Tab,
    pub(super) games: Vec<UiGame>,
    pub(super) daemon_socket: PathBuf,
    pub(super) daemon_connected: Option<bool>,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) launching: Option<String>,
    pub(super) selected: Option<String>,
    pub(super) draft: Option<Draft>,
    pub(super) saving: bool,
    pub(super) saved_msg: Option<String>,
    /// Library search query (matches name or exe path).
    pub(super) search: String,
    pub(super) confirm_delete: bool,
    /// "Add game" tab state (manual entry — no scanning).
    pub(super) new_name: String,
    pub(super) new_game_dir: String,
    pub(super) new_exe: String,
    pub(super) creating: bool,
    pub(super) create_msg: Option<String>,
    /// Settings tab: wine prefix.
    pub(super) wine_prefix_input: String,
    /// Set when the user edits the prefix by hand, so a `wine.status` reply that
    /// was already in flight cannot overwrite it.
    pub(super) wine_prefix_dirty: bool,
    pub(super) wine_status: Option<WineStatus>,
    pub(super) wine_msg: Option<String>,
    /// Automatic reconnect bookkeeping.
    pub(super) retry_attempts: u32,
    /// Live sessions by game id (refreshed periodically).
    pub(super) running: std::collections::BTreeMap<String, SessionInfo>,
    /// Settings tab: cloud sync.
    pub(super) sync_status: Option<SyncStatus>,
    pub(super) sync_form: SyncForm,
    /// Restore waiting for a second click: (game id, snapshot).
    pub(super) sync_restore_pending: Option<(String, Option<String>)>,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let socket = crate::config::socket_path();

        (
            Self {
                tab: Tab::Games,
                games: Vec::new(),
                daemon_socket: socket,
                daemon_connected: None,
                loading: false,
                error: None,
                launching: None,
                selected: None,
                draft: None,
                saving: false,
                saved_msg: None,
                search: String::new(),
                confirm_delete: false,
                new_name: String::new(),
                new_game_dir: String::new(),
                new_exe: String::new(),
                creating: false,
                create_msg: None,
                wine_prefix_input: String::new(),
                wine_prefix_dirty: false,
                wine_status: None,
                wine_msg: None,
                retry_attempts: 0,
                running: std::collections::BTreeMap::new(),
                sync_status: None,
                sync_form: SyncForm::default(),
                sync_restore_pending: None,
            },
            Task::batch([
                Task::perform(async { connect_and_load().await }, Message::GamesLoaded),
                Task::perform(
                    async { load_wine_status().await },
                    Message::WineStatusLoaded,
                ),
                Task::perform(
                    async { load_sync_status().await },
                    Message::SyncStatusLoaded,
                ),
                // Start the periodic session poll.
                Task::perform(async { tokio::time::sleep(STATUS_POLL).await }, |_| {
                    Message::Tick
                }),
            ]),
        )
    }

    /// Minimum master password length as reported by the daemon (with a sane
    /// fallback so the form is usable before the first status arrives).
    pub(super) fn min_master_password(&self) -> usize {
        self.sync_status
            .as_ref()
            .map(|status| status.min_master_password)
            .filter(|minimum| *minimum > 0)
            .unwrap_or(8)
    }

    /// Encryption as last reported by the daemon, used to decide whether a save
    /// is an encryption *change* (which the daemon will ask about).
    pub(super) fn stored_encryption(&self) -> bool {
        self.sync_status
            .as_ref()
            .and_then(|status| status.settings.get("encryption"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Re-read the sync status after a change.
    pub(super) fn reload_sync(&self) -> Task<Message> {
        Task::perform(
            async { load_sync_status().await },
            Message::SyncStatusLoaded,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture};

    #[test]
    fn a_late_status_reply_never_eats_what_the_user_typed() {
        let (mut app, _task) = App::new();
        let status = sync_status_fixture();
        // Opening the settings tab fires a `sync.status` request...
        app.sync_form.apply(&status, &sync_payload()["settings"]);

        // ...and while it is in flight the user pastes their keys by hand.
        for message in [
            Message::SyncField(SyncField::KeyId, "0046b5".into()),
            Message::SyncField(SyncField::AppKey, "K004bk5u".into()),
            Message::SyncField(SyncField::Bucket, "my-own-bucket".into()),
            Message::SyncToggleEnabled(false),
        ] {
            let _ = app.update(message);
        }

        // The reply lands about a second later: it must not wipe the form.
        let _ = app.update(Message::SyncStatusLoaded(Ok(status)));

        assert_eq!(app.sync_form.key_id, "0046b5");
        assert_eq!(app.sync_form.app_key, "K004bk5u");
        assert_eq!(app.sync_form.bucket, "my-own-bucket");
        assert!(!app.sync_form.enabled);
        assert_eq!(app.sync_form.keep_versions, "0");
    }

    #[test]
    fn saving_consumes_the_secrets_it_was_given_and_no_more() {
        let (mut app, _task) = App::new();
        let status = sync_status_fixture();
        app.sync_form.apply(&status, &sync_payload()["settings"]);
        for message in [
            Message::SyncField(SyncField::KeyId, "0046b5".into()),
            Message::SyncField(SyncField::AppKey, "K004bk5u".into()),
            Message::SyncField(SyncField::Password, "hunter2hunter2".into()),
            Message::SyncField(SyncField::Bucket, "my-own-bucket".into()),
        ] {
            let _ = app.update(message);
        }

        // A successful save echoes nothing back and clears only what it took.
        let _ = app.update(Message::SyncCredentialsSaved(Ok(())));
        assert!(app.sync_form.key_id.is_empty());
        assert!(app.sync_form.app_key.is_empty());
        assert_eq!(app.sync_form.password, "hunter2hunter2");

        // Saving the sync password clears its pair, and leaves the (still
        // unsaved) bucket edit alone even though a reload follows.
        let _ = app.update(Message::SyncPasswordSaved(Ok(())));
        assert!(app.sync_form.password.is_empty());
        assert!(app.sync_form.password_again.is_empty());
        assert_eq!(app.sync_form.bucket, "my-own-bucket");

        // Once the settings are saved, the daemon is the truth again.
        let _ = app.update(Message::SyncSettingsSaved(Ok(())));
        assert!(!app.sync_form.settings_dirty);
        app.sync_form.apply(&status, &sync_payload()["settings"]);
        assert_eq!(app.sync_form.bucket, "kotori-saves");

        // A failed save keeps the user's text so they can correct it.
        let _ = app.update(Message::SyncField(SyncField::Bucket, "typo".into()));
        let _ = app.update(Message::SyncSettingsSaved(Err("boom".into())));
        app.sync_form.apply(&status, &sync_payload()["settings"]);
        assert_eq!(app.sync_form.bucket, "typo");
    }

    #[test]
    fn a_late_wine_status_reply_does_not_clobber_a_typed_prefix() {
        let (mut app, _task) = App::new();
        let _ = app.update(Message::WinePrefixChanged("/prefixes/mine".into()));
        let _ = app.update(Message::WineStatusLoaded(Ok(WineStatus {
            configured: Some("/home/user/.wine".into()),
            default_prefix: "/home/user/.wine".into(),
            environment: None,
            detected: vec!["/home/user/.wine".into()],
        })));
        assert_eq!(app.wine_prefix_input, "/prefixes/mine");

        // Once saved, the daemon's answer may fill the field again.
        let _ = app.update(Message::WinePrefixSaved(Ok(())));
        let _ = app.update(Message::WineStatusLoaded(Ok(WineStatus {
            configured: Some("/prefixes/mine".into()),
            default_prefix: "/prefixes/mine".into(),
            environment: None,
            detected: vec![],
        })));
        assert_eq!(app.wine_prefix_input, "/prefixes/mine");
        assert!(!app.wine_prefix_dirty);
    }

    #[test]
    fn deleting_a_credential_takes_effect_at_once_and_says_so() {
        let (mut app, _task) = App::new();
        app.sync_form
            .apply(&sync_status_fixture(), &sync_payload()["settings"]);
        let _ = app.update(Message::SyncField(SyncField::KeyId, "0046b5".into()));
        let _ = app.update(Message::SyncField(SyncField::AppKey, "K004".into()));

        // Deleting does not require emptying the boxes first.
        let _ = app.update(Message::SyncClearCredentials);
        assert!(app.sync_form.busy);
        let _ = app.update(Message::SyncCredentialsCleared(Ok(())));
        assert!(!app.sync_form.busy);
        assert!(app.sync_form.key_id.is_empty() && app.sync_form.app_key.is_empty());
        assert_eq!(
            app.sync_form.msg.as_deref(),
            Some("已删除密钥环里的 B2 凭据")
        );

        // A failure keeps what the user typed and names the problem.
        let _ = app.update(Message::SyncField(
            SyncField::Password,
            "hunter2hunter2".into(),
        ));
        let _ = app.update(Message::SyncClearPassword);
        let _ = app.update(Message::SyncPasswordCleared(Err("密钥环没在运行".into())));
        assert_eq!(app.sync_form.password, "hunter2hunter2");
        assert!(
            app.sync_form
                .msg
                .as_deref()
                .unwrap_or_default()
                .contains("密钥环没在运行")
        );
    }
}

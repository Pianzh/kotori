use std::path::{Path, PathBuf};

use iced::widget::{
    button, column, container, horizontal_rule, pick_list, row, scrollable, slider, text,
    text_input, toggler,
};
use iced::{Color, Element, Length, Size, Task, Theme};
use serde_json::Value;

use crate::config::{ScaleAlgorithm, ScaleProfile};

const UI_FONT_FAMILY: &str = "Kotori Sans";
const UI_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/KotoriSans-Regular.ttf");

fn ui_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(UI_FONT_FAMILY),
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Games,
    Add,
    Settings,
}

/// Wine prefix situation on this machine (settings page).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WineStatus {
    pub configured: Option<String>,
    pub default_prefix: String,
    pub environment: Option<String>,
    pub detected: Vec<String>,
}

/// Maximum number of automatic reconnect attempts before giving up (a manual
/// "重连" always works, and resets the counter).
const MAX_AUTO_RETRIES: u32 = 5;

/// How often the UI polls the daemon for live sessions.
const STATUS_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// The three save-location kinds, as shown in the editor.
const SAVE_PATH_KINDS: [&str; 3] = ["windows", "relative", "absolute"];

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
    fn last_label(&self) -> String {
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
    fn has_secret(&self, account: &str) -> bool {
        self.secrets.iter().any(|a| a == account)
    }
}

/// Which sync input a keystroke went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncField {
    Endpoint,
    Bucket,
    Prefix,
    KeepVersions,
    KeyId,
    AppKey,
    Password,
    PasswordAgain,
}

/// The editable half of the sync settings.
#[derive(Debug, Clone, Default)]
struct SyncForm {
    loaded: bool,
    /// Set as soon as the user edits a *settings* field. `sync.status` replies
    /// can land seconds after the request (the daemon probes the keyring on the
    /// way), so a reply that was already in flight must never overwrite what
    /// the user is in the middle of typing. Cleared once a save succeeds.
    settings_dirty: bool,
    enabled: bool,
    endpoint: String,
    bucket: String,
    prefix: String,
    keep_versions: String,
    encryption: bool,
    key_id: String,
    app_key: String,
    password: String,
    password_again: String,
    /// Master password for the credential file (unlock, or set one up).
    master_password: String,
    /// An encryption change needs one more click: it decides whether existing
    /// data in the bucket can still be read.
    confirm_encryption: Option<bool>,
    msg: Option<String>,
    busy: bool,
}

impl SyncForm {
    /// Fill the form from what the daemon reports. Secrets are never echoed, so
    /// their inputs are left alone here: they are only cleared when a save
    /// actually consumed them (`SyncCredentialsSaved` / `SyncPasswordSaved`).
    ///
    /// Everything is skipped while `settings_dirty` is set — see the field.
    fn apply(&mut self, status: &SyncStatus, settings: &Value) {
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
    fn patch(&self, force: bool) -> Value {
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

/// Text field of a JSON object, or empty when absent.
fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
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

impl UiGame {
    fn scale_label(&self) -> String {
        format!(
            "{}  {}x{} -> {}x{}",
            self.algo, self.internal.0, self.internal.1, self.output.0, self.output.1
        )
    }
}

/// Editable copy of a game's scale profile.
#[derive(Debug, Clone)]
struct Draft {
    game_id: String,
    game_name: String,
    profile_name: String,
    /// Editable game root and exe path, plus their stored values so unchanged
    /// fields are not re-sent (the daemon rejects a path that does not exist,
    /// e.g. when the game lives on a drive that is not mounted right now).
    game_dir: String,
    game_dir_original: String,
    exe: String,
    exe_original: String,
    save_paths: Vec<SavePathDraft>,
    save_paths_original: Vec<SavePathDraft>,
    algo: String,
    sharpness: u32,
    internal_w: String,
    internal_h: String,
    output_w: String,
    output_h: String,
    /// Kept as text so a half-typed ratio survives an edit. Empty means "no
    /// ratio". The widget for it arrives with the scale-section rework; until
    /// then this only carries the stored value through an open + save.
    scale_ratio: String,
    follow_window: bool,
    fullscreen: bool,
    framerate: String,
}

impl Draft {
    /// Seed the form from the *stored* profile. Anything else means a plain
    /// "open + save" silently rewrites the user's settings.
    fn from_game(game: &UiGame) -> Self {
        Self {
            game_id: game.id.clone(),
            game_name: game.name.clone(),
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
    fn exe_changed(&self) -> bool {
        self.exe.trim() != self.exe_original
    }

    /// Has the user changed the game root?
    fn game_dir_changed(&self) -> bool {
        self.game_dir.trim() != self.game_dir_original
    }

    /// Has the user changed the save locations?
    fn save_paths_changed(&self) -> bool {
        self.save_paths != self.save_paths_original
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TabChanged(Tab),
    Refresh,
    GamesLoaded(Result<Vec<UiGame>, String>),
    Launch(String),
    LaunchDone(Result<Value, String>),
    GameSelected(String),
    BackToList,
    SearchChanged(String),
    AlgoChanged(String),
    SharpnessChanged(f32),
    InternalWChanged(String),
    InternalHChanged(String),
    OutputWChanged(String),
    OutputHChanged(String),
    ScaleRatioChanged(String),
    FullscreenToggled(bool),
    FramerateChanged(String),
    ExePathChanged(String),
    GameDirChanged(String),
    SavePathKindChanged(usize, String),
    SavePathChanged(usize, String),
    SavePathExcludeChanged(usize, String),
    AddSavePath,
    RemoveSavePath(usize),
    SaveProfile,
    ProfileSaved(Result<(), String>),
    DeleteRequested,
    DeleteCancelled,
    DeleteConfirmed,
    Deleted(Result<(), String>),
    NewNameChanged(String),
    NewGameDirChanged(String),
    NewExeChanged(String),
    CreateRequested,
    CreateFinished(Result<String, String>),
    WinePrefixChanged(String),
    SaveWinePrefix,
    ClearWinePrefix,
    WinePrefixSaved(Result<(), String>),
    WineStatusLoaded(Result<WineStatus, String>),
    StatusLoaded(Result<std::collections::BTreeMap<String, SessionInfo>, String>),
    Stop(String),
    StopDone(Result<(), String>),
    Tick,

    // --- cloud sync (settings tab) ---
    SyncStatusLoaded(Result<SyncStatus, String>),
    SyncToggleEnabled(bool),
    SyncEncryptionToggled(bool),
    SyncConfirmEncryption,
    SyncCancelEncryption,
    SyncField(SyncField, String),
    SyncSaveSettings,
    SyncSettingsSaved(Result<(), String>),
    SyncSaveCredentials,
    SyncCredentialsSaved(Result<(), String>),
    SyncClearCredentials,
    SyncCredentialsCleared(Result<(), String>),
    SyncSavePassword,
    SyncPasswordSaved(Result<(), String>),
    SyncClearPassword,
    SyncPasswordCleared(Result<(), String>),
    SyncTest,
    SyncTested(Result<String, String>),
    SyncMasterPasswordChanged(String),
    SyncUnlock,
    SyncUnlocked(Result<(), String>),
    SyncSetMasterPassword,
    SyncMasterSaved(Result<String, String>),
    SyncNow(Option<String>),
    SyncNowDone(Result<String, String>),
    SyncRestoreRequested(String, Option<String>),
    SyncRestoreCancelled,
    SyncRestoreConfirmed,
}

pub struct App {
    tab: Tab,
    games: Vec<UiGame>,
    daemon_socket: PathBuf,
    daemon_connected: Option<bool>,
    loading: bool,
    error: Option<String>,
    launching: Option<String>,
    selected: Option<String>,
    draft: Option<Draft>,
    saving: bool,
    saved_msg: Option<String>,
    /// Library search query (matches name or exe path).
    search: String,
    confirm_delete: bool,
    /// "Add game" tab state (manual entry — no scanning).
    new_name: String,
    new_game_dir: String,
    new_exe: String,
    creating: bool,
    create_msg: Option<String>,
    /// Settings tab: wine prefix.
    wine_prefix_input: String,
    /// Set when the user edits the prefix by hand, so a `wine.status` reply that
    /// was already in flight cannot overwrite it.
    wine_prefix_dirty: bool,
    wine_status: Option<WineStatus>,
    wine_msg: Option<String>,
    /// Automatic reconnect bookkeeping.
    retry_attempts: u32,
    /// Live sessions by game id (refreshed periodically).
    running: std::collections::BTreeMap<String, SessionInfo>,
    /// Settings tab: cloud sync.
    sync_status: Option<SyncStatus>,
    sync_form: SyncForm,
    /// Restore waiting for a second click: (game id, snapshot).
    sync_restore_pending: Option<(String, Option<String>)>,
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

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabChanged(tab) => {
                self.tab = tab;
                self.selected = None;
                self.draft = None;
                self.confirm_delete = false;
                self.error = None;
                if tab == Tab::Settings {
                    // Re-read both, they may have changed on disk.
                    return Task::batch([
                        Task::perform(
                            async { load_wine_status().await },
                            Message::WineStatusLoaded,
                        ),
                        Task::perform(
                            async { load_sync_status().await },
                            Message::SyncStatusLoaded,
                        ),
                    ]);
                }
                Task::none()
            }
            Message::Refresh => {
                self.error = None;
                self.loading = true;
                Task::perform(async { connect_and_load().await }, Message::GamesLoaded)
            }
            Message::GamesLoaded(Ok(games)) => {
                self.games = games;
                self.loading = false;
                self.daemon_connected = Some(true);
                self.error = None;
                self.retry_attempts = 0;
                Task::none()
            }
            Message::GamesLoaded(Err(e)) => {
                self.loading = false;
                self.daemon_connected = Some(false);
                self.error = Some(e);
                // Self-heal: keep retrying with backoff, so the UI recovers on
                // its own once the daemon is back.
                self.retry_attempts = self.retry_attempts.saturating_add(1);
                if self.retry_attempts <= MAX_AUTO_RETRIES {
                    let delay = retry_delay(self.retry_attempts);
                    return Task::perform(async move { tokio::time::sleep(delay).await }, |_| {
                        Message::Refresh
                    });
                }
                Task::none()
            }
            Message::Launch(id) => {
                self.launching = Some(id.clone());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move {
                        let mut params = serde_json::Map::new();
                        params.insert("id".into(), Value::String(id));
                        crate::rpc::call(&socket, "game.launch", Some(params)).await
                    },
                    Message::LaunchDone,
                )
            }
            Message::LaunchDone(result) => {
                self.launching = None;
                match result {
                    Ok(value) => {
                        let sid = value
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        tracing::info!("game session started: {sid}");
                        self.error = None;
                        let socket = self.daemon_socket.clone();
                        return Task::perform(
                            async move { load_status(&socket).await },
                            Message::StatusLoaded,
                        );
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::GameSelected(id) => {
                if let Some(g) = self.games.iter().find(|g| g.id == id) {
                    self.selected = Some(g.id.clone());
                    self.saved_msg = None;
                    self.confirm_delete = false;
                    // Seed the form from the *stored* profile. Anything else
                    // means a plain "open + save" silently rewrites settings.
                    self.draft = Some(Draft::from_game(g));
                }
                Task::none()
            }
            Message::BackToList => {
                self.selected = None;
                self.draft = None;
                self.saved_msg = None;
                self.confirm_delete = false;
                Task::none()
            }
            Message::SearchChanged(query) => {
                self.search = query;
                Task::none()
            }
            Message::AlgoChanged(algo) => {
                if let Some(d) = &mut self.draft {
                    d.algo = algo;
                }
                Task::none()
            }
            Message::SharpnessChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.sharpness = v.round() as u32;
                }
                Task::none()
            }
            Message::InternalWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_w = v;
                }
                Task::none()
            }
            Message::InternalHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_h = v;
                }
                Task::none()
            }
            Message::OutputWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_w = v;
                }
                Task::none()
            }
            Message::OutputHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_h = v;
                }
                Task::none()
            }
            Message::ScaleRatioChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.scale_ratio = v;
                }
                Task::none()
            }
            Message::FullscreenToggled(b) => {
                if let Some(d) = &mut self.draft {
                    d.fullscreen = b;
                }
                Task::none()
            }
            Message::FramerateChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.framerate = v;
                }
                Task::none()
            }
            Message::ExePathChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.exe = v;
                }
                Task::none()
            }
            Message::DeleteRequested => {
                self.confirm_delete = true;
                Task::none()
            }
            Message::DeleteCancelled => {
                self.confirm_delete = false;
                Task::none()
            }
            Message::DeleteConfirmed => {
                let Some(game_id) = self.selected.clone() else {
                    return Task::none();
                };
                self.confirm_delete = false;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { remove_game(&socket, &game_id).await },
                    Message::Deleted,
                )
            }
            Message::Deleted(result) => {
                match result {
                    Ok(()) => {
                        self.selected = None;
                        self.draft = None;
                        self.error = None;
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::NewNameChanged(value) => {
                self.new_name = value;
                Task::none()
            }
            Message::NewGameDirChanged(value) => {
                self.new_game_dir = value;
                Task::none()
            }
            Message::NewExeChanged(value) => {
                self.new_exe = value;
                Task::none()
            }
            Message::CreateRequested => {
                let name = self.new_name.trim().to_string();
                let exe = self.new_exe.trim().to_string();
                let game_dir = self.new_game_dir.trim().to_string();
                if name.is_empty() || exe.is_empty() {
                    self.create_msg = Some("游戏名和可执行文件都必须填写".to_string());
                    return Task::none();
                }
                self.creating = true;
                self.create_msg = None;
                self.error = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { create_game(&socket, name, exe, game_dir).await },
                    Message::CreateFinished,
                )
            }
            Message::CreateFinished(result) => {
                self.creating = false;
                match result {
                    Ok(id) => {
                        self.create_msg = Some(format!("已添加（ID: {id}），可在游戏库里继续配置"));
                        self.new_name.clear();
                        self.new_game_dir.clear();
                        self.new_exe.clear();
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::WineStatusLoaded(result) => {
                match result {
                    Ok(status) => {
                        // Do not clobber an edit that is still in progress: this
                        // reply can arrive a second after the user started typing.
                        if !self.wine_prefix_dirty {
                            self.wine_prefix_input = status.configured.clone().unwrap_or_default();
                        }
                        self.wine_status = Some(status);
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::WinePrefixChanged(value) => {
                self.wine_prefix_input = value;
                self.wine_prefix_dirty = true;
                Task::none()
            }
            Message::SaveWinePrefix => {
                let prefix = self.wine_prefix_input.trim().to_string();
                self.wine_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_wine_prefix(&socket, Some(prefix)).await },
                    Message::WinePrefixSaved,
                )
            }
            Message::ClearWinePrefix => {
                self.wine_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_wine_prefix(&socket, None).await },
                    Message::WinePrefixSaved,
                )
            }
            Message::WinePrefixSaved(result) => {
                self.wine_msg = Some(match &result {
                    Ok(()) => "已保存".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon holds it now, so a reload may refill the field.
                    self.wine_prefix_dirty = false;
                } else if let Err(e) = &result {
                    self.error = Some(e.clone());
                }
                Task::perform(
                    async { load_wine_status().await },
                    Message::WineStatusLoaded,
                )
            }
            Message::GameDirChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.game_dir = value;
                }
                Task::none()
            }
            Message::SavePathKindChanged(index, kind) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.kind = kind;
                }
                Task::none()
            }
            Message::SavePathChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.path = value;
                }
                Task::none()
            }
            Message::SavePathExcludeChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.exclude = value;
                }
                Task::none()
            }
            Message::AddSavePath => {
                if let Some(draft) = &mut self.draft {
                    draft.save_paths.push(SavePathDraft {
                        kind: "windows".to_string(),
                        path: "%APPDATA%\\".to_string(),
                        exclude: String::new(),
                    });
                }
                Task::none()
            }
            Message::RemoveSavePath(index) => {
                if let Some(draft) = &mut self.draft
                    && index < draft.save_paths.len()
                {
                    draft.save_paths.remove(index);
                }
                Task::none()
            }
            Message::Tick => {
                let socket = self.daemon_socket.clone();
                let poll = Task::perform(
                    async move { load_status(&socket).await },
                    Message::StatusLoaded,
                );
                let next = Task::perform(async { tokio::time::sleep(STATUS_POLL).await }, |_| {
                    Message::Tick
                });
                Task::batch([poll, next])
            }
            Message::StatusLoaded(Ok(running)) => {
                self.running = running;
                self.daemon_connected = Some(true);
                Task::none()
            }
            Message::StatusLoaded(Err(e)) => {
                // Do not fight the reconnect loop for the error banner; just
                // mark the daemon as gone and let the user see it.
                self.daemon_connected = Some(false);
                tracing::debug!("status poll failed: {e}");
                Task::none()
            }

            Message::SyncStatusLoaded(Ok(status)) => {
                self.sync_form.apply(&status, &status.settings);
                self.sync_status = Some(status);
                Task::none()
            }
            Message::SyncStatusLoaded(Err(e)) => {
                self.sync_form.loaded = true;
                self.sync_form.msg = Some(format!("读取同步状态失败: {e}"));
                Task::none()
            }
            Message::SyncToggleEnabled(value) => {
                self.sync_form.enabled = value;
                self.sync_form.settings_dirty = true;
                Task::none()
            }
            // Flipping encryption decides whether data already in the bucket can
            // be read at all, so it asks once more instead of taking effect.
            Message::SyncEncryptionToggled(value) => {
                if value == self.sync_form.encryption {
                    self.sync_form.confirm_encryption = None;
                } else {
                    self.sync_form.confirm_encryption = Some(value);
                    self.sync_form.msg = Some(if value {
                        "开启加密后，bucket 里已有的明文存档将读不出来（除非换一个 prefix）。再点一次「确认开启」才会生效。".to_string()
                    } else {
                        "关闭加密后，之前加密上传的存档将无法解密。再点一次「确认关闭」才会生效。"
                            .to_string()
                    });
                }
                Task::none()
            }
            Message::SyncConfirmEncryption => {
                if let Some(value) = self.sync_form.confirm_encryption.take() {
                    self.sync_form.encryption = value;
                    self.sync_form.settings_dirty = true;
                    self.sync_form.msg = Some("已勾选，记得点「保存设置」".to_string());
                }
                Task::none()
            }
            Message::SyncCancelEncryption => {
                self.sync_form.confirm_encryption = None;
                self.sync_form.msg = None;
                Task::none()
            }
            Message::SyncField(field, value) => {
                let form = &mut self.sync_form;
                match field {
                    SyncField::Endpoint => form.endpoint = value,
                    SyncField::Bucket => form.bucket = value,
                    SyncField::Prefix => form.prefix = value,
                    SyncField::KeepVersions => form.keep_versions = value,
                    SyncField::KeyId => form.key_id = value,
                    SyncField::AppKey => form.app_key = value,
                    SyncField::Password => form.password = value,
                    SyncField::PasswordAgain => form.password_again = value,
                }
                if matches!(
                    field,
                    SyncField::Endpoint
                        | SyncField::Bucket
                        | SyncField::Prefix
                        | SyncField::KeepVersions
                ) {
                    form.settings_dirty = true;
                }
                Task::none()
            }
            Message::SyncSaveSettings => {
                let force = self.sync_form.confirm_encryption.is_some()
                    || self.sync_form.encryption != self.stored_encryption();
                let form = self.sync_form.clone();
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_settings(&socket, form.patch(force)).await },
                    Message::SyncSettingsSaved,
                )
            }
            Message::SyncSettingsSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    // The daemon refuses an encryption flip that is not confirmed;
                    // show its words rather than a generic failure.
                    Ok(()) => "已保存".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon now holds exactly what the form holds, so a
                    // later status reply may refill the form again.
                    self.sync_form.settings_dirty = false;
                } else {
                    self.sync_form.confirm_encryption = None;
                }
                self.reload_sync()
            }
            Message::SyncSaveCredentials => {
                let key_id = self.sync_form.key_id.trim().to_string();
                let app_key = self.sync_form.app_key.trim().to_string();
                if key_id.is_empty() && app_key.is_empty() {
                    self.sync_form.msg = Some("两个字段都空着：这只会清掉已保存的凭据".to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_credentials(&socket, &key_id, &app_key).await },
                    Message::SyncCredentialsSaved,
                )
            }
            Message::SyncCredentialsSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => "凭据已存入系统密钥环（磁盘上没有明文）".to_string(),
                    Err(e) => format!("保存凭据失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon consumed them; never echo secrets back.
                    self.sync_form.key_id.clear();
                    self.sync_form.app_key.clear();
                }
                self.reload_sync()
            }
            Message::SyncClearCredentials => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_credentials(&socket, "", "").await },
                    Message::SyncCredentialsCleared,
                )
            }
            Message::SyncCredentialsCleared(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => "已删除密钥环里的 B2 凭据".to_string(),
                    Err(e) => format!("删除凭据失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.key_id.clear();
                    self.sync_form.app_key.clear();
                }
                self.reload_sync()
            }
            Message::SyncClearPassword => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_password(&socket, "").await },
                    Message::SyncPasswordCleared,
                )
            }
            Message::SyncPasswordCleared(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => "已删除密钥环里的同步密码".to_string(),
                    Err(e) => format!("删除密码失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.password.clear();
                    self.sync_form.password_again.clear();
                }
                self.reload_sync()
            }
            Message::SyncSavePassword => {
                let password = self.sync_form.password.clone();
                if !password.is_empty() && password != self.sync_form.password_again {
                    self.sync_form.msg = Some("两次输入的密码不一样".to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_password(&socket, &password).await },
                    Message::SyncPasswordSaved,
                )
            }
            Message::SyncPasswordSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) if self.sync_form.password.is_empty() => "已清除同步密码".to_string(),
                    Ok(()) => "密码已存入系统密钥环（我们不会替你生成密码）".to_string(),
                    Err(e) => format!("保存密码失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.password.clear();
                    self.sync_form.password_again.clear();
                }
                self.reload_sync()
            }
            Message::SyncTest => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(async move { sync_test(&socket).await }, Message::SyncTested)
            }
            Message::SyncTested(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(remote) => format!("连接正常：{remote}"),
                    Err(e) => format!("连接失败: {e}"),
                });
                Task::none()
            }
            Message::SyncMasterPasswordChanged(value) => {
                self.sync_form.master_password = value;
                Task::none()
            }
            Message::SyncUnlock => {
                let password = self.sync_form.master_password.clone();
                if password.is_empty() {
                    self.sync_form.msg = Some("请先输入主密码".to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { unlock_credentials(&socket, &password).await },
                    Message::SyncUnlocked,
                )
            }
            Message::SyncUnlocked(result) => {
                self.sync_form.busy = false;
                match result {
                    Ok(()) => {
                        self.sync_form.master_password.clear();
                        self.sync_form.msg = Some("已解锁".to_string());
                    }
                    Err(e) => self.sync_form.msg = Some(e),
                }
                self.reload_sync()
            }
            Message::SyncSetMasterPassword => {
                let password = self.sync_form.master_password.clone();
                if password.chars().count() < self.min_master_password() {
                    self.sync_form.msg = Some(format!(
                        "主密码至少要 {} 个字符",
                        self.min_master_password()
                    ));
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_master_password(&socket, &password).await },
                    Message::SyncMasterSaved,
                )
            }
            Message::SyncMasterSaved(result) => {
                self.sync_form.busy = false;
                match result {
                    Ok(path) => {
                        self.sync_form.master_password.clear();
                        self.sync_form.msg = Some(format!("凭据已加密保存到 {path}"));
                    }
                    Err(e) => self.sync_form.msg = Some(e),
                }
                self.reload_sync()
            }
            Message::SyncNow(game_id) => {
                self.sync_form.busy = true;
                self.sync_form.msg = Some("正在同步…".to_string());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_now(&socket, game_id).await },
                    Message::SyncNowDone,
                )
            }
            Message::SyncNowDone(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(summary) => summary,
                    Err(e) => format!("同步失败: {e}"),
                });
                self.reload_sync()
            }
            Message::SyncRestoreRequested(game_id, version) => {
                self.sync_restore_pending = Some((game_id, version));
                Task::none()
            }
            Message::SyncRestoreCancelled => {
                self.sync_restore_pending = None;
                Task::none()
            }
            Message::SyncRestoreConfirmed => {
                let Some((game_id, version)) = self.sync_restore_pending.take() else {
                    return Task::none();
                };
                self.sync_form.busy = true;
                self.sync_form.msg = Some("正在恢复…".to_string());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_restore(&socket, &game_id, version.as_deref()).await },
                    Message::SyncNowDone,
                )
            }
            Message::Stop(game_id) => {
                let Some(session) = self.running.get(&game_id).map(|s| s.session_id.clone()) else {
                    return Task::none();
                };
                let socket = self.daemon_socket.clone();
                self.error = None;
                Task::perform(
                    async move { stop_session(&socket, &session).await },
                    Message::StopDone,
                )
            }
            Message::StopDone(result) => {
                if let Err(e) = result {
                    self.error = Some(e);
                }
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { load_status(&socket).await },
                    Message::StatusLoaded,
                )
            }
            Message::SaveProfile => {
                let Some(draft) = self.draft.clone() else {
                    return Task::none();
                };
                self.saving = true;
                self.saved_msg = None;
                Task::perform(
                    async move { save_profile(draft).await },
                    Message::ProfileSaved,
                )
            }
            Message::ProfileSaved(result) => {
                self.saving = false;
                self.saved_msg = Some(match &result {
                    Ok(()) => "已保存并通知守护进程".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if let Err(e) = &result {
                    self.error = Some(e.clone());
                } else {
                    // Refresh the library so the new scale shows up.
                    return Task::perform(async { connect_and_load().await }, |r| {
                        Message::GamesLoaded(r)
                    });
                }
                Task::none()
            }
        }
    }

    /// Minimum master password length as reported by the daemon (with a sane
    /// fallback so the form is usable before the first status arrives).
    fn min_master_password(&self) -> usize {
        self.sync_status
            .as_ref()
            .map(|status| status.min_master_password)
            .filter(|minimum| *minimum > 0)
            .unwrap_or(8)
    }

    /// Encryption as last reported by the daemon, used to decide whether a save
    /// is an encryption *change* (which the daemon will ask about).
    fn stored_encryption(&self) -> bool {
        self.sync_status
            .as_ref()
            .and_then(|status| status.settings.get("encryption"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Re-read the sync status after a change.
    fn reload_sync(&self) -> Task<Message> {
        Task::perform(
            async { load_sync_status().await },
            Message::SyncStatusLoaded,
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let content = match self.tab {
            Tab::Games => {
                if self.selected.is_some() {
                    self.game_detail_view()
                } else {
                    self.games_view()
                }
            }
            Tab::Settings => self.settings_view(),
            Tab::Add => self.add_view(),
        };

        row![
            self.sidebar(),
            container(content)
                .padding(18)
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .height(Length::Fill)
        .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let daemon_status = match self.daemon_connected {
            Some(true) => ("已连接", Color::from_rgb8(0x4c, 0xaf, 0x50)),
            Some(false) if self.retry_attempts <= MAX_AUTO_RETRIES && self.retry_attempts > 0 => {
                ("未连接（重试中…）", Color::from_rgb8(0xe5, 0x39, 0x35))
            }
            Some(false) => ("未连接", Color::from_rgb8(0xe5, 0x39, 0x35)),
            None => ("检测中...", Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        };

        let mut status_row = row![
            text("\u{25CF}").size(12).color(daemon_status.1),
            text(daemon_status.0)
                .size(11)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        if self.daemon_connected == Some(false) {
            status_row = status_row.push(
                button(text("重连").size(11))
                    .padding([3, 8])
                    .on_press(Message::Refresh),
            );
        }

        container(
            column![
                text("Kotori").size(20).font(ui_font()),
                horizontal_rule(1),
                self.nav_item(Tab::Games, "游戏库"),
                self.nav_item(Tab::Add, "添加游戏"),
                self.nav_item(Tab::Settings, "设置"),
                iced::widget::Space::with_height(Length::Fill),
                status_row,
            ]
            .spacing(4)
            .padding(16),
        )
        .width(200)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(Color::from_rgb8(0x1b, 0x1e, 0x24).into()),
            ..Default::default()
        })
        .into()
    }

    fn nav_item(&self, tab: Tab, label: &'static str) -> Element<'_, Message> {
        let active = self.tab == tab;
        button(text(label).size(15))
            .padding([10, 14])
            .width(Length::Fill)
            .on_press(Message::TabChanged(tab))
            .style(move |_t: &Theme, _s: iced::widget::button::Status| {
                iced::widget::button::Style {
                    background: Some(
                        if active {
                            Color::from_rgb8(0x2c, 0x6b, 0xbf)
                        } else {
                            Color::TRANSPARENT
                        }
                        .into(),
                    ),
                    text_color: if active {
                        Color::WHITE
                    } else {
                        Color::from_rgb8(0xc8, 0xc8, 0xc8)
                    },
                    ..Default::default()
                }
            })
            .into()
    }

    fn games_view(&self) -> Element<'_, Message> {
        let visible: Vec<&UiGame> = self
            .games
            .iter()
            .filter(|g| matches_query(g, &self.search))
            .collect();

        let count = if self.search.trim().is_empty() {
            format!("游戏库 ({})", self.games.len())
        } else {
            format!("游戏库 ({} / {})", visible.len(), self.games.len())
        };
        let refresh_btn = button(text(if self.loading {
            "加载中..."
        } else {
            "刷新"
        }))
        .on_press(Message::Refresh);

        let search_input = text_input("搜索游戏名或路径…", &self.search)
            .on_input(Message::SearchChanged)
            .padding([7, 10]);

        let mut list = column![
            row![
                text(count).size(18).font(ui_font()),
                iced::widget::horizontal_space(),
                refresh_btn,
            ]
            .align_y(iced::Alignment::Center),
            search_input,
        ]
        .spacing(8);

        if let Some(err) = &self.error {
            list = list.push(
                container(
                    text(format!("\u{26A0} {err}"))
                        .size(13)
                        .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
                )
                .padding(10)
                .width(Length::Fill)
                .style(|_theme: &Theme| iced::widget::container::Style {
                    background: Some(Color::from_rgb8(0x3a, 0x22, 0x22).into()),
                    border: iced::border::Border::default().rounded(6),
                    ..Default::default()
                }),
            );
        }

        if self.games.is_empty() && self.error.is_none() {
            list = list.push(
                text(if self.loading {
                    "正在从守护进程加载游戏列表..."
                } else {
                    "还没有游戏。切到「添加游戏」扫描一个目录，或执行 `kotori scan <目录>`。"
                })
                .size(13)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        } else if visible.is_empty() {
            list = list.push(
                text(format!("没有匹配「{}」的游戏", self.search.trim()))
                    .size(13)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        }

        if self.games.is_empty() {
            list.into()
        } else {
            scrollable(
                list.push(
                    column(
                        visible
                            .iter()
                            .map(|g| self.game_card(g))
                            .collect::<Vec<_>>(),
                    )
                    .spacing(8),
                ),
            )
            .into()
        }
    }

    /// Manual add: name + game root + executable. No scanning, no guessing.
    fn add_view(&self) -> Element<'_, Message> {
        let create_btn = button(text(if self.creating {
            "添加中…"
        } else {
            "添加游戏"
        }))
        .padding([8, 20])
        .on_press(Message::CreateRequested);

        let mut body = column![
            text("添加游戏").size(18).font(ui_font()),
            horizontal_rule(1),
            text("手动填写。游戏根目录是启动时的工作目录，也是存档相对路径的基准；留空则取可执行文件所在目录。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            labeled_input("游戏名", "例如 3days", &self.new_name, Message::NewNameChanged),
            labeled_input(
                "游戏根目录",
                "/path/to/game",
                &self.new_game_dir,
                Message::NewGameDirChanged,
            ),
            labeled_input(
                "可执行文件",
                "/path/to/game.exe",
                &self.new_exe,
                Message::NewExeChanged,
            ),
            {
                let status: Element<'_, Message> = match &self.create_msg {
                    Some(msg) => text(msg)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0xda, 0xa5))
                        .into(),
                    None => iced::widget::Space::new(0, 0).into(),
                };
                row![create_btn, status]
                    .spacing(12)
                    .align_y(iced::Alignment::Center)
            },
            text("添加后可在游戏库的详情页里继续配置缩放、Wine 目录与存档位置。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
        ]
        .spacing(10);

        if let Some(err) = &self.error {
            body = body.push(
                text(format!("\u{26A0} {err}"))
                    .size(12)
                    .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
            );
        }

        scrollable(body).into()
    }

    fn game_card(&self, game: &UiGame) -> Element<'_, Message> {
        let launching = self.launching.as_deref() == Some(game.id.as_str());
        let session = self.running.get(&game.id);

        // A live session turns the action button into "stop"; a watch-only game
        // that is not running yet offers "monitor" instead of "launch".
        let action_btn = if session.is_some() {
            button(text("停止"))
                .padding([8, 18])
                .on_press(Message::Stop(game.id.clone()))
                .style(
                    |_t: &Theme, _s: iced::widget::button::Status| iced::widget::button::Style {
                        background: Some(Color::from_rgb8(0x8c, 0x3b, 0x3b).into()),
                        text_color: Color::WHITE,
                        ..Default::default()
                    },
                )
        } else {
            let label = if launching {
                "启动中…"
            } else if game.watch_only {
                "监视"
            } else {
                "启动"
            };
            button(text(label))
                .padding([8, 18])
                .on_press(Message::Launch(game.id.clone()))
                .style(move |_t: &Theme, _s: iced::widget::button::Status| {
                    iced::widget::button::Style {
                        background: Some(
                            if launching {
                                Color::from_rgb8(0x37, 0x40, 0x51)
                            } else {
                                Color::from_rgb8(0x2c, 0x6b, 0xbf)
                            }
                            .into(),
                        ),
                        text_color: Color::WHITE,
                        ..Default::default()
                    }
                })
        };

        let status_badge: Element<'_, Message> = match session {
            Some(session) if session.watch_only => text("● 监视中")
                .size(11)
                .color(Color::from_rgb8(0xd8, 0xa6, 0x57))
                .into(),
            Some(_) => text("● 运行中")
                .size(11)
                .color(Color::from_rgb8(0x4c, 0xaf, 0x50))
                .into(),
            None => iced::widget::Space::new(0, 0).into(),
        };

        let edit_btn = button(text("配置"))
            .padding([8, 14])
            .on_press(Message::GameSelected(game.id.clone()));

        let mut details = column![
            text(game.name.clone()).size(15).font(ui_font()),
            text(game.exe.clone())
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            text(game.scale_label())
                .size(11)
                .color(Color::from_rgb8(0x7a, 0xaa, 0x7f)),
        ]
        .spacing(3)
        .align_x(iced::Alignment::Start);

        if game.watch_only {
            details = details.push(
                text(format!(
                    "仅观测：由你自行启动，kotori 跟随进程 {}",
                    if game.process_name.is_empty() {
                        "（未设置）"
                    } else {
                        &game.process_name
                    }
                ))
                .size(11)
                .color(Color::from_rgb8(0xd8, 0xa6, 0x57)),
            );
        }

        container(
            row![
                details,
                iced::widget::horizontal_space(),
                status_badge,
                edit_btn,
                action_btn,
            ]
            .align_y(iced::Alignment::Center)
            .padding([14, 14])
            .spacing(8),
        )
        .width(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(Color::from_rgb8(0x22, 0x27, 0x2e).into()),
            border: iced::border::Border::default().rounded(8),
            ..Default::default()
        })
        .into()
    }

    fn game_detail_view(&self) -> Element<'_, Message> {
        let Some(draft) = &self.draft else {
            return self.games_view();
        };

        let back_btn = button(text("← 返回")).on_press(Message::BackToList);

        let algo_options: Vec<String> = ScaleAlgorithm::ALL.iter().map(|s| s.to_string()).collect();
        let algo_pick: iced::widget::PickList<
            '_,
            String,
            Vec<String>,
            String,
            Message,
            Theme,
            iced::Renderer,
        > = pick_list(algo_options, Some(draft.algo.clone()), |algo| {
            Message::AlgoChanged(algo)
        });

        let show_sharpness = matches!(draft.algo.as_str(), "Fsr" | "Nis");
        let sharpness_row: Element<'_, Message> = if show_sharpness {
            let sharp: iced::widget::Slider<'_, f32, Message> =
                slider(0.0..=5.0, draft.sharpness as f32, Message::SharpnessChanged);
            row![
                text("锐度").size(13).width(80),
                sharp.width(200),
                text(format!("{}", draft.sharpness))
                    .size(13)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into()
        } else {
            iced::widget::row![].into()
        };

        let num_input = |label: &'static str,
                         value: String,
                         msg: fn(String) -> Message|
         -> Element<'_, Message> {
            row![
                text(label).size(13).width(120),
                text_input("", &value)
                    .on_input(msg)
                    .padding([6, 8])
                    .width(120),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(8)
            .into()
        };

        let fullscreen_toggle: Element<'_, Message> = row![
            text("全屏启动").size(13).width(120),
            toggler(draft.fullscreen).on_toggle(Message::FullscreenToggled),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let framerate_input: Element<'_, Message> = row![
            text("帧率限制 (留空不限)").size(13).width(120),
            text_input("60", &draft.framerate)
                .on_input(Message::FramerateChanged)
                .padding([6, 8])
                .width(120),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let game_dir_row: Element<'_, Message> = row![
            text("游戏根目录").size(13).width(120),
            text_input("/path/to/game", &draft.game_dir)
                .on_input(Message::GameDirChanged)
                .padding([6, 8])
                .width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let exe_row: Element<'_, Message> = row![
            text("可执行文件").size(13).width(120),
            text_input("/path/to/game.exe", &draft.exe)
                .on_input(Message::ExePathChanged)
                .padding([6, 8])
                .width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let save_btn = button(text(if self.saving {
            "保存中..."
        } else {
            "保存配置"
        }))
        .padding([10, 24])
        .on_press(Message::SaveProfile);

        let algo_row: Element<'_, Message> = iced::widget::Row::new()
            .push(text("缩放算法").size(13).width(120))
            .push(algo_pick)
            .align_y(iced::Alignment::Center)
            .spacing(8)
            .into();

        // Save locations: three kinds, each optionally with exclude patterns.
        let mut save_rows = column![].spacing(6);
        for (index, entry) in draft.save_paths.iter().enumerate() {
            let kind_pick = pick_list(
                SAVE_PATH_KINDS.map(str::to_string).to_vec(),
                Some(entry.kind.clone()),
                move |kind| Message::SavePathKindChanged(index, kind),
            );
            save_rows = save_rows.push(
                row![
                    kind_pick,
                    text_input(kind_placeholder(&entry.kind), &entry.path)
                        .on_input(move |value| Message::SavePathChanged(index, value))
                        .padding([6, 8])
                        .width(Length::Fill),
                    text_input("排除：*.log, cache/", &entry.exclude)
                        .on_input(move |value| Message::SavePathExcludeChanged(index, value))
                        .padding([6, 8])
                        .width(190),
                    button(text("删除").size(11))
                        .padding([6, 10])
                        .on_press(Message::RemoveSavePath(index)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }

        let mut body = column![
            row![back_btn, iced::widget::horizontal_space()],
            text(&draft.game_name)
                .size(20)
                .font(ui_font()),
            text("每次修改保存后，重新启动游戏即生效。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            horizontal_rule(1),
            game_dir_row,
            exe_row,
            algo_row,
            sharpness_row,
            num_input("游戏分辨率宽", draft.internal_w.clone(), Message::InternalWChanged),
            num_input("游戏分辨率高", draft.internal_h.clone(), Message::InternalHChanged),
            num_input("输出分辨率宽", draft.output_w.clone(), Message::OutputWChanged),
            num_input("输出分辨率高", draft.output_h.clone(), Message::OutputHChanged),
            num_input("缩放倍数", draft.scale_ratio.clone(), Message::ScaleRatioChanged),
            text("缩放倍数 = 输出像素 ÷ 游戏自身分辨率，填了它就以它为准（输出分辨率宽/高只作为参考显示）。启动游戏时按这个倍数开窗，快捷键「按设定比例缩放／取消缩放」（默认 Shift+Alt+Q）也是在这个倍数和 1:1 之间来回切。留空则沿用输出分辨率。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            text("提示：在 Niri 等平铺桌面下游戏会铺满整块显示器，输出分辨率主要影响缩放计算；窗口缩放在 KDE 上通过 KWin 完成。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            fullscreen_toggle,
            framerate_input,
            horizontal_rule(1),
            text("存档位置").size(15).font(ui_font()),
            text("windows = prefix 内的 Windows 路径，推荐用 %APPDATA% / %DOCUMENTS% / %SAVEDGAMES% 令牌（不要写 C:\\users\\<用户名>，各 prefix 的用户名不一样）；relative = 相对游戏根目录；absolute = 仅本机，不跨平台同步。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            save_rows,
            button(text("添加存档位置").size(12))
                .padding([6, 12])
                .on_press(Message::AddSavePath),
            horizontal_rule(1),
            {
                let save_status: Element<'_, Message> = match &self.saved_msg {
                    Some(msg) => {
                        text(msg).size(12).color(Color::from_rgb8(0x9e, 0xda, 0xa5)).into()
                    }
                    None => iced::widget::Space::new(0, 0).into(),
                };
                row![save_btn, save_status]
                    .align_y(iced::Alignment::Center)
                    .spacing(12)
            },
            text("提示：游戏窗口聚焦时，可用 gamescope 快捷键实时切换：Super+U FSR、Super+Y NIS、Super+N 最近邻、Super+I/O 锐度增/减。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            horizontal_rule(1),
            {
                // Deleting only drops the library entry, never the game files.
                let delete_area: Element<'_, Message> = if self.confirm_delete {
                    row![
                        text("删除这个条目？（不会删除游戏文件）")
                            .size(12)
                            .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
                        button(text("确认删除"))
                            .padding([6, 14])
                            .on_press(Message::DeleteConfirmed),
                        button(text("取消"))
                            .padding([6, 14])
                            .on_press(Message::DeleteCancelled),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
                } else {
                    button(text("删除条目"))
                        .padding([6, 14])
                        .on_press(Message::DeleteRequested)
                        .into()
                };
                delete_area
            },
        ]
        .spacing(10);

        if self.games.is_empty() {
            body = body.push(
                text("等待加载...")
                    .size(12)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        }

        scrollable(body).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let gray = Color::from_rgb8(0x9e, 0x9e, 0x9e);
        let dim = Color::from_rgb8(0x8a, 0x8a, 0x8a);

        let effective = self
            .wine_status
            .as_ref()
            .and_then(|status| status.configured.clone())
            .unwrap_or_else(|| "自动探测".to_string());
        let default_prefix = self
            .wine_status
            .as_ref()
            .map(|status| status.default_prefix.clone())
            .unwrap_or_else(|| "读取中…".to_string());
        let environment = self
            .wine_status
            .as_ref()
            .and_then(|status| status.environment.clone())
            .unwrap_or_else(|| "未设置".to_string());
        let detected: Vec<String> = self
            .wine_status
            .as_ref()
            .map(|status| status.detected.clone())
            .unwrap_or_default();

        let mut detected_list = column![].spacing(3);
        if detected.is_empty() {
            detected_list =
                detected_list.push(text("没有在常见位置发现 wine prefix").size(11).color(dim));
        } else {
            for prefix in detected {
                detected_list = detected_list.push(text(format!("· {prefix}")).size(11).color(dim));
            }
        }

        let mut body = column![
            text("设置").size(18).font(ui_font()),
            horizontal_rule(1),
            text("Wine 目录（prefix）").size(15).font(ui_font()),
            text("启动游戏时使用。留空 = 自动探测：游戏目录内的可携式 prefix → 常见位置 → ~/.wine。每个游戏也可以在详情页里单独覆盖。")
                .size(12)
                .color(gray),
            row![
                text_input("留空即自动探测", &self.wine_prefix_input)
                    .on_input(Message::WinePrefixChanged)
                    .padding([7, 10])
                    .width(Length::Fill),
                button(text("保存")).padding([8, 18]).on_press(Message::SaveWinePrefix),
                button(text("自动")).padding([8, 14]).on_press(Message::ClearWinePrefix),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            {
                let status: Element<'_, Message> = match &self.wine_msg {
                    Some(msg) => text(msg)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0xda, 0xa5))
                        .into(),
                    None => iced::widget::Space::new(0, 0).into(),
                };
                status
            },
            text(format!("当前生效：{effective}")).size(12).color(dim),
            text(format!("默认位置：{default_prefix}")).size(11).color(dim),
            text(format!("WINEPREFIX 环境变量：{environment}")).size(11).color(dim),
            text("自动探测到的 prefix：").size(11).color(dim),
            detected_list,
            horizontal_rule(1),
        ]
        .spacing(10);

        for section in self.sync_sections(gray, dim) {
            body = body.push(section);
        }

        if let Some(err) = &self.error {
            body = body.push(
                text(format!("\u{26A0} {err}"))
                    .size(12)
                    .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
            );
        }

        scrollable(body).into()
    }

    /// The cloud-sync half of the settings page.
    ///
    /// Built as separate pieces so each one can be read on its own: what state
    /// the account is in, what the settings are, the two secrets, and what has
    /// been synced.
    fn sync_sections(&self, gray: Color, dim: Color) -> Vec<Element<'_, Message>> {
        let form = &self.sync_form;
        let status = self.sync_status.as_ref();
        let ok = Color::from_rgb8(0x9e, 0xda, 0xa5);
        let warn = Color::from_rgb8(0xef, 0xc0, 0x7a);

        let mut sections = vec![
            text("云存档同步").size(15).font(ui_font()).into(),
            text(
                "存档通过 rclone 传到 Backblaze B2。默认不加密：bucket 里的存档就是普通文件，                 用任何 S3 工具都能取回，不需要 kotori，也不需要 rclone。",
            )
            .size(12)
            .color(gray)
            .into(),
        ];

        // --- state ---------------------------------------------------------
        let mut state = column![row![
            text("启用云同步").size(13),
            toggler(form.enabled)
                .on_toggle(Message::SyncToggleEnabled)
                .size(16),
            iced::widget::Space::new(Length::Fill, 0),
            button(text("测试连接"))
                .padding([6, 12])
                .on_press_maybe((!form.busy).then_some(Message::SyncTest)),
            button(text("立即同步全部"))
                .padding([6, 12])
                .on_press_maybe((!form.busy).then_some(Message::SyncNow(None))),
        ]]
        .spacing(10);

        match status {
            None => {
                state = state.push(text("读取中…").size(11).color(dim));
            }
            Some(status) => {
                state = state.push(text(format!("远端：{}", status.remote)).size(11).color(dim));
                match &status.rclone {
                    Some(path) => {
                        state = state.push(text(format!("rclone：{path}")).size(11).color(dim))
                    }
                    None => {
                        state = state.push(
                            text("rclone 未安装（Arch：sudo pacman -S rclone）")
                                .size(11)
                                .color(warn),
                        )
                    }
                }
                state = state.push(
                    text(format!("密钥环：{}", status.keyring))
                        .size(11)
                        .color(if status.ephemeral { warn } else { dim }),
                );
                if status.ephemeral {
                    state = state.push(
                        text("⚠ 本机没有运行中的系统密钥环，填进去的凭据只留在内存里，重启后要重新输入。")
                            .size(11)
                            .color(warn),
                    );
                    // The platform-specific advice lives in one place
                    // (`secrets::keyring_hint`); the UI only relays it.
                    state = state.push(text(crate::secrets::keyring_hint()).size(11).color(dim));
                }
                if let Some(problem) = &status.problem {
                    state = state.push(text(format!("待解决：{problem}")).size(11).color(warn));
                } else if status.ready {
                    state = state.push(text("✓ 已就绪").size(11).color(ok));
                }
            }
        }
        sections.push(state.into());

        // --- settings ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("连接与保留").size(13).font(ui_font()).into());
        sections.push(
            row![
                text("bucket").size(13).width(120),
                text_input("B2 上那个 bucket 的名字", &form.bucket)
                    .on_input(|v| Message::SyncField(SyncField::Bucket, v))
                    .padding([7, 10])
                    .width(Length::Fill),
                text("prefix").size(13).width(50),
                text_input("kotori", &form.prefix)
                    .on_input(|v| Message::SyncField(SyncField::Prefix, v))
                    .padding([7, 10])
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );
        sections.push(sync_input_row(
            "API endpoint",
            "留空即可（rclone 会自己找到）",
            &form.endpoint,
            SyncField::Endpoint,
            false,
        ));
        sections.push(
            text("prefix 是 bucket 里归 kotori 独占的目录，bucket 里的其他东西我们一律不碰。")
                .size(11)
                .color(dim)
                .into(),
        );
        sections.push(
            row![
                text("保留版本数").size(13).width(120),
                text_input("0 = 永久保留", &form.keep_versions)
                    .on_input(|v| Message::SyncField(SyncField::KeepVersions, v))
                    .padding([7, 10])
                    .width(120),
                text("0 表示永不删除云端快照；填写后只清理旧快照，绝不动本地存档。")
                    .size(11)
                    .color(dim),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        let mut encryption = row![
            text("加密上传（rclone crypt）").size(13),
            toggler(form.encryption)
                .on_toggle(Message::SyncEncryptionToggled)
                .size(16),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);
        if let Some(pending) = form.confirm_encryption {
            encryption = encryption.push(
                button(text(if pending {
                    "确认开启"
                } else {
                    "确认关闭"
                }))
                .padding([6, 12])
                .on_press(Message::SyncConfirmEncryption),
            );
            encryption = encryption.push(
                button(text("取消"))
                    .padding([6, 12])
                    .on_press(Message::SyncCancelEncryption),
            );
        }
        sections.push(encryption.into());
        sections.push(
            text("关闭时存档是明文文件（推荐）；开启后必须记住密码，忘了就打不开自己的备份。")
                .size(11)
                .color(dim)
                .into(),
        );

        sections.push(
            row![
                button(text("保存设置"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSaveSettings)),
                {
                    let msg: Element<'_, Message> = match &form.msg {
                        Some(msg) => text(msg.clone()).size(11).color(ok).into(),
                        None => iced::widget::Space::new(0, 0).into(),
                    };
                    msg
                },
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        // --- credentials ---------------------------------------------------
        // --- where the credentials live ------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("凭据存储").size(13).font(ui_font()).into());
        let store = status.map(|s| s.store_kind.as_str()).unwrap_or("");
        sections.push(
            text(status.map(|s| s.keyring.clone()).unwrap_or_default())
                .size(11)
                .color(dim)
                .into(),
        );

        match store {
            // A file on disk, sealed with a password only the user knows.
            "encrypted-file" => {
                if status.map(|s| s.store_locked).unwrap_or(false) {
                    sections.push(
                        text("凭据文件已锁定：输入主密码解锁（解锁后本次守护进程内一直有效）。")
                            .size(11)
                            .color(warn)
                            .into(),
                    );
                    sections.push(
                        row![
                            text("主密码").size(13).width(120),
                            text_input("凭据文件的主密码", &form.master_password)
                                .on_input(Message::SyncMasterPasswordChanged)
                                .secure(true)
                                .padding([7, 10])
                                .width(Length::Fill),
                            button(text("解锁"))
                                .padding([7, 16])
                                .on_press_maybe((!form.busy).then_some(Message::SyncUnlock)),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .into(),
                    );
                } else {
                    sections.push(text("✓ 已解锁").size(11).color(ok).into());
                }
            }
            // Nothing on this machine can persist a secret: say so, explain how
            // to fix the machine, and offer the way out that works anywhere.
            "session-only" => {
                sections.push(
                    text("⚠ 本机没有运行中的密钥环，凭据只留在内存里，重启后要重新输入。")
                        .size(11)
                        .color(warn)
                        .into(),
                );
                sections.push(
                    text(crate::secrets::keyring_hint())
                        .size(11)
                        .color(dim)
                        .into(),
                );
                sections.push(
                    text(format!(
                        "也可以在这里设一个主密码：凭据会用 Argon2id + ChaCha20-Poly1305 加密存到 {}，\
                         之后每次开机只要输一次主密码。密码由你自己保管，我们不会存它。",
                        status
                            .map(|s| s.store_path.clone())
                            .filter(|path| !path.is_empty())
                            .unwrap_or_else(|| "凭据文件".to_string())
                    ))
                    .size(11)
                    .color(dim)
                    .into(),
                );
                let hint = format!("至少 {} 位，自己记得住就行", self.min_master_password());
                sections.push(
                    row![
                        text("主密码").size(13).width(120),
                        text_input(hint.as_str(), &form.master_password)
                            .on_input(Message::SyncMasterPasswordChanged)
                            .secure(true)
                            .padding([7, 10])
                            .width(Length::Fill),
                        button(text("加密保存凭据"))
                            .padding([7, 16])
                            .on_press_maybe((!form.busy).then_some(Message::SyncSetMasterPassword)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center)
                    .into(),
                );
            }
            _ => {}
        }

        sections.push(horizontal_rule(1).into());
        sections.push(
            text("B2 凭据（只保存一套，再次保存即覆盖）")
                .size(13)
                .font(ui_font())
                .into(),
        );
        sections.push(
            text(
                "第一次用 B2 的话，先在网页控制台做两件事：\n                 1. Buckets → Create a Bucket，名字填到上面的 bucket 里（Files in Bucket 选 Private）\n                 2. Account → Application Keys → Add a New Application Key：Bucket(s) 只勾这一个 bucket，\n                 \u{20}\u{20}\u{20}Type of Access 选 Read and Write\n                 创建后会显示 keyID 和 applicationKey，只显示这一次，复制到下面两个框里。",
            )
            .size(11)
            .color(dim)
            .into(),
        );
        let known = |account: &str| status.is_some_and(|s| s.has_secret(account));
        let key_id_saved = known("b2-key-id");
        let app_key_saved = known("b2-app-key");
        let stored = usize::from(key_id_saved) + usize::from(app_key_saved);
        sections.push(
            text(credentials_label(key_id_saved, app_key_saved))
                .size(11)
                .color(if stored == 2 { ok } else { warn })
                .into(),
        );
        sections.push(sync_input_row(
            "keyID",
            "Application Key ID（形如 005a…）",
            &form.key_id,
            SyncField::KeyId,
            false,
        ));
        sections.push(sync_input_row(
            "applicationKey",
            "只在创建时显示一次，丢了就再建一个",
            &form.app_key,
            SyncField::AppKey,
            true,
        ));
        sections.push(
            row![
                button(text("保存凭据"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSaveCredentials)),
                // Deleting takes effect immediately: no need to empty the boxes
                // and save, which looked like it might do nothing.
                button(text("删除凭据")).padding([7, 16]).on_press_maybe(
                    (!form.busy && stored > 0).then_some(Message::SyncClearCredentials)
                ),
                text("两个框填的都是同一个 B2 账号，下次保存会覆盖上一套")
                    .size(11)
                    .color(dim),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        // --- password ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(
            text("同步密码（仅在开启加密时使用）")
                .size(13)
                .font(ui_font())
                .into(),
        );
        sections.push(
            text(
                "密码由你自己设定，我们不会替你生成——你从没见过的密码就等于把备份锁在别人手里。                 它只进系统密钥环；忘了也能自己取回来：",
            )
            .size(11)
            .color(dim)
            .into(),
        );
        sections.push(
            text(status.map(|s| s.password_hint.clone()).unwrap_or_default())
                .size(11)
                .color(gray)
                .font(iced::Font::MONOSPACE)
                .into(),
        );
        sections.push(
            row![
                text("密码").size(13).width(120),
                text_input("留空 = 清除密码", &form.password)
                    .on_input(|v| Message::SyncField(SyncField::Password, v))
                    .secure(true)
                    .padding([7, 10])
                    .width(Length::Fill),
                text("再输一次").size(13).width(70),
                text_input("确认", &form.password_again)
                    .on_input(|v| Message::SyncField(SyncField::PasswordAgain, v))
                    .secure(true)
                    .padding([7, 10])
                    .width(Length::Fill),
                button(text("保存密码"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSavePassword)),
                button(text("删除密码")).padding([7, 16]).on_press_maybe(
                    (!form.busy && known("sync-password")).then_some(Message::SyncClearPassword)
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );
        sections.push(
            text(format!(
                "密码状态：{}",
                if known("sync-password") {
                    "已保存在密钥环（改密码会让已加密上传的存档无法解密，会再确认一次）"
                } else {
                    "未设置（没开加密就不需要它）"
                }
            ))
            .size(11)
            .color(dim)
            .into(),
        );

        // --- per game ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("各游戏存档").size(13).font(ui_font()).into());
        let games = status.map(|s| s.games.clone()).unwrap_or_default();
        if games.is_empty() {
            sections.push(text("还没有游戏").size(11).color(dim).into());
        }
        for game in games {
            let pending = self
                .sync_restore_pending
                .as_ref()
                .is_some_and(|(id, _)| *id == game.id);
            let mut line = row![
                text(game.name.clone())
                    .size(12)
                    .width(Length::FillPortion(3)),
                text(format!("{} 个位置", game.locations))
                    .size(11)
                    .color(dim)
                    .width(Length::FillPortion(1)),
                text(game.last_label())
                    .size(11)
                    .color(dim)
                    .width(Length::FillPortion(3)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);

            if game.locations > 0 && !game.problem.is_some() {
                line = line.push(button(text("同步")).padding([4, 10]).on_press_maybe(
                    (!form.busy).then_some(Message::SyncNow(Some(game.id.clone()))),
                ));
                if pending {
                    line = line.push(
                        button(text("确认恢复（会覆盖本地存档）"))
                            .padding([4, 10])
                            .on_press(Message::SyncRestoreConfirmed),
                    );
                    line = line.push(
                        button(text("取消"))
                            .padding([4, 10])
                            .on_press(Message::SyncRestoreCancelled),
                    );
                } else {
                    line = line.push(
                        button(text("恢复")).padding([4, 10]).on_press_maybe(
                            (!form.busy)
                                .then_some(Message::SyncRestoreRequested(game.id.clone(), None)),
                        ),
                    );
                }
            } else if game.locations == 0 {
                line = line.push(text("还没配置存档位置").size(11).color(dim));
            }

            sections.push(line.into());
            if let Some(problem) = &game.problem {
                sections.push(text(format!("    ⚠ {problem}")).size(11).color(warn).into());
            }
        }

        sections
    }
}

/// What the B2 section says about the stored credentials.
///
/// There is only ever **one** set of B2 keys (the daemon overwrites the two
/// keyring entries), so the useful information is how many of its two halves
/// are actually there — never the values, which do not leave the keyring.
fn credentials_label(has_key_id: bool, has_app_key: bool) -> String {
    let stored = usize::from(has_key_id) + usize::from(has_app_key);
    let mark = |saved: bool| if saved { "✓" } else { "✗ 未保存" };
    format!(
        "密钥环里现在有 {stored}/2 项：keyID {}，applicationKey {}。再次保存会覆盖上一套，\
         只存进密钥环，配置文件里没有任何明文。",
        mark(has_key_id),
        mark(has_app_key),
    )
}

/// Label + (optionally masked) input row for the sync form.
fn sync_input_row<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
    field: SyncField,
    secret: bool,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(120),
        text_input(placeholder, value)
            .on_input(move |v| Message::SyncField(field, v))
            .secure(secret)
            .padding([7, 10])
            .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Label + text input row used by the add form.
fn labeled_input<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
    on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(100),
        text_input(placeholder, value)
            .on_input(on_input)
            .padding([7, 10])
            .width(Length::Fill),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(8)
    .into()
}

/// Load the library, booting the daemon first if it is not running. Used for
/// both the initial load and automatic reconnect.
async fn connect_and_load() -> Result<Vec<UiGame>, String> {
    let socket = crate::config::socket_path();
    match load_games_from(&socket).await {
        Ok(games) => Ok(games),
        Err(first) => {
            let boot = socket.clone();
            let booted = tokio::task::spawn_blocking(move || crate::daemon::ensure_running(&boot))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("启动守护进程的任务失败: {e}")));
            match booted {
                Ok(()) => load_games_from(&socket).await,
                Err(_) => Err(first),
            }
        }
    }
}

async fn load_games_from(socket: &Path) -> Result<Vec<UiGame>, String> {
    let value = crate::rpc::call(socket, "game.list", None).await?;
    parse_games(&value)
}

/// Backoff for automatic reconnect attempts: 2s, 4s, 8s, 16s, capped at 30s.
fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.pow(attempt.min(4)).min(30))
}

/// Case-insensitive match against a game's name or exe path; an empty query
/// matches everything.
fn matches_query(game: &UiGame, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    game.name.to_lowercase().contains(&query) || game.exe.to_lowercase().contains(&query)
}

/// Add one game from explicit user input.
async fn create_game(
    socket: &Path,
    name: String,
    exe_path: String,
    game_dir: String,
) -> Result<String, String> {
    let mut params = vec![
        ("name", Value::String(name)),
        ("exe_path", Value::String(exe_path)),
    ];
    let game_dir = game_dir.trim();
    if !game_dir.is_empty() {
        params.push(("game_dir", Value::String(game_dir.to_string())));
    }

    let value = crate::rpc::call(socket, "game.create", Some(crate::rpc::params(params))).await?;
    Ok(value
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string())
}

/// `Some(prefix)` sets the machine-wide wine prefix, `None` returns to
/// auto-detection.
async fn set_wine_prefix(socket: &Path, prefix: Option<String>) -> Result<(), String> {
    let value = match prefix {
        Some(prefix) => Value::String(prefix),
        None => Value::Null,
    };
    crate::rpc::call(
        socket,
        "wine.set_prefix",
        Some(crate::rpc::params([("prefix", value)])),
    )
    .await?;
    Ok(())
}

async fn load_wine_status() -> Result<WineStatus, String> {
    let value = crate::rpc::call(&crate::config::socket_path(), "wine.status", None).await?;
    Ok(parse_wine_status(&value))
}

fn parse_wine_status(value: &Value) -> WineStatus {
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    WineStatus {
        configured: text("configured"),
        default_prefix: text("default").unwrap_or_default(),
        environment: text("environment"),
        detected: value
            .get("detected")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Read the cloud-sync status. Secrets are never returned by the daemon, so
/// this can be held in the UI without any caution.
async fn load_sync_status() -> Result<SyncStatus, String> {
    let socket = crate::config::socket_path();
    let value = crate::rpc::call(&socket, "sync.status", None).await?;
    parse_sync_status(&value)
}

fn parse_sync_status(value: &Value) -> Result<SyncStatus, String> {
    if value.get("settings").is_none() {
        return Err("守护进程没有返回同步设置".to_string());
    }
    let games = value
        .get("games")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| SyncGameRow {
                    id: str_field(row, "id"),
                    name: str_field(row, "name"),
                    locations: row.get("locations").and_then(|v| v.as_u64()).unwrap_or(0),
                    problem: row
                        .get("location_problem")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    last: row.get("last").and_then(|last| {
                        if last.is_null() {
                            return None;
                        }
                        let action = last.get("action").and_then(|v| v.as_str()).unwrap_or("");
                        let detail = last.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                        let when = last
                            .get("at")
                            .and_then(|v| v.as_str())
                            .map(|at| at.chars().take(16).collect::<String>())
                            .unwrap_or_default();
                        let mark = if last.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "✓"
                        } else {
                            "✗"
                        };
                        Some(format!("{mark} {when} {action} {detail}"))
                    }),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SyncStatus {
        settings: value.get("settings").cloned().unwrap_or(Value::Null),
        remote: str_field(value, "remote"),
        rclone: value
            .get("rclone")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        keyring: value
            .get("keyring")
            .map(|keyring| str_field(keyring, "backend"))
            .unwrap_or_default(),
        store_kind: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "kind"))
            .unwrap_or_default(),
        store_locked: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .and_then(|store| store.get("locked"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        store_path: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "path"))
            .unwrap_or_default(),
        min_master_password: value
            .get("keyring")
            .and_then(|keyring| keyring.get("min_master_password"))
            .and_then(|v| v.as_u64())
            .unwrap_or(8) as usize,
        ephemeral: value
            .get("keyring")
            .and_then(|v| v.get("ephemeral"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        secrets: string_list(value.get("secrets")),
        ready: value
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        problem: value
            .get("problem")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        password_hint: str_field(value, "password_hint"),
        games,
    })
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the sync settings. The daemon validates and may refuse (an
/// unconfirmed encryption change, an impossible prefix), so its message is
/// surfaced verbatim.
async fn save_sync_settings(socket: &Path, patch: Value) -> Result<(), String> {
    let params = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "内部错误：设置补丁不是对象".to_string())?;
    crate::rpc::call(socket, "sync.set_settings", Some(params)).await?;
    Ok(())
}

async fn save_sync_credentials(socket: &Path, key_id: &str, app_key: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.set_credentials",
        Some(crate::rpc::params([
            ("key_id", Value::String(key_id.to_string())),
            ("app_key", Value::String(app_key.to_string())),
        ])),
    )
    .await?;
    Ok(())
}

async fn save_sync_password(socket: &Path, password: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.set_password",
        Some(crate::rpc::params([
            ("password", Value::String(password.to_string())),
            // Changing an existing password under encryption is confirmed in
            // the UI; the daemon only insists on an explicit intent.
            ("force", Value::Bool(true)),
        ])),
    )
    .await?;
    Ok(())
}

/// Unlock the master-password file. The password goes over IPC to our own
/// daemon and is never written anywhere.
async fn unlock_credentials(socket: &Path, password: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.unlock",
        Some(crate::rpc::params([(
            "password",
            Value::String(password.to_string()),
        )])),
    )
    .await?;
    Ok(())
}

/// Seal the current credentials into a master-password file, and say where it
/// landed.
async fn set_master_password(socket: &Path, password: &str) -> Result<String, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_master_password",
        Some(crate::rpc::params([
            ("password", Value::String(password.to_string())),
            // The UI asked for the password in a dedicated field; that is the
            // confirmation.
            ("force", Value::Bool(true)),
        ])),
    )
    .await?;
    Ok(str_field(&value, "path"))
}

async fn sync_test(socket: &Path) -> Result<String, String> {
    let value = crate::rpc::call(socket, "sync.test", None).await?;
    Ok(str_field(&value, "remote"))
}

/// Upload now, and turn the daemon's per-location report into one line.
async fn sync_now(socket: &Path, game_id: Option<String>) -> Result<String, String> {
    let params = crate::rpc::params(game_id.map(|id| ("id", Value::String(id))));
    let value = crate::rpc::call(socket, "sync.now", Some(params)).await?;
    Ok(describe_sync_outcome(&value))
}

async fn sync_restore(
    socket: &Path,
    game_id: &str,
    version: Option<&str>,
) -> Result<String, String> {
    let mut params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    if let Some(version) = version {
        params.insert("version".into(), Value::String(version.to_string()));
    }
    let value = crate::rpc::call(socket, "sync.restore", Some(params)).await?;
    Ok(describe_sync_outcome(&value["game"]))
}

/// One line summarising a sync result: how many locations moved, or what broke.
fn describe_sync_outcome(value: &Value) -> String {
    let outcomes: Vec<&Value> = match (
        value.get("games").and_then(|v| v.as_array()),
        value.get("game").filter(|v| !v.is_null()),
    ) {
        (Some(games), _) => games.iter().collect(),
        (None, Some(game)) => vec![game],
        _ => vec![value],
    };

    let mut moved = 0usize;
    let mut skipped = 0usize;
    let mut problems = Vec::new();
    for outcome in &outcomes {
        if let Some(error) = outcome.get("error").and_then(|v| v.as_str()) {
            problems.push(error.to_string());
        }
        for location in outcome
            .get("locations")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            match location.get("action").and_then(|v| v.as_str()) {
                Some("skipped") => skipped += 1,
                Some("failed") => {}
                _ => moved += 1,
            }
        }
    }

    if !problems.is_empty() {
        return format!("失败：{}", problems.join("；"));
    }
    if moved == 0 {
        return format!("没有需要同步的变化（跳过 {skipped} 个位置）");
    }
    format!("完成：{moved} 个位置已同步，跳过 {skipped} 个")
}

/// Live sessions, keyed by game id.
async fn load_status(
    socket: &Path,
) -> Result<std::collections::BTreeMap<String, SessionInfo>, String> {
    let value = crate::rpc::call(socket, "daemon.status", None).await?;
    let mut running = std::collections::BTreeMap::new();

    if let Some(sessions) = value.get("sessions").and_then(|v| v.as_array()) {
        for session in sessions {
            let (Some(game_id), Some(session_id)) = (
                session.get("game_id").and_then(|v| v.as_str()),
                session.get("session_id").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            running.insert(
                game_id.to_string(),
                SessionInfo {
                    session_id: session_id.to_string(),
                    watch_only: session
                        .get("watch_only")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                },
            );
        }
    }

    Ok(running)
}

async fn stop_session(socket: &Path, session_id: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "game.stop",
        Some(crate::rpc::params([(
            "session_id",
            Value::String(session_id.to_string()),
        )])),
    )
    .await?;
    Ok(())
}

/// Placeholder that shows the expected shape of each save-location kind.
fn kind_placeholder(kind: &str) -> &'static str {
    match kind {
        "windows" => "%APPDATA%\\Game\\save",
        "absolute" => "/home/user/saves/game",
        _ => "savedata",
    }
}

fn save_paths_to_json(paths: &[SavePathDraft]) -> Value {
    Value::Array(
        paths
            .iter()
            .map(|entry| {
                let mut object = serde_json::Map::new();
                object.insert("kind".into(), Value::String(entry.kind.clone()));
                object.insert("path".into(), Value::String(entry.path.clone()));
                let exclude: Vec<Value> = entry
                    .exclude
                    .split(',')
                    .map(str::trim)
                    .filter(|pattern| !pattern.is_empty())
                    .map(|pattern| Value::String(pattern.to_string()))
                    .collect();
                if !exclude.is_empty() {
                    object.insert("exclude".into(), Value::Array(exclude));
                }
                Value::Object(object)
            })
            .collect(),
    )
}

fn parse_save_paths(value: Option<&Value>) -> Vec<SavePathDraft> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| SavePathDraft {
                    kind: item
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("relative")
                        .to_string(),
                    path: item
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    exclude: item
                        .get("exclude")
                        .and_then(|v| v.as_array())
                        .map(|patterns| {
                            patterns
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn remove_game(socket: &Path, game_id: &str) -> Result<(), String> {
    let params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    crate::rpc::call(socket, "game.remove", Some(params)).await?;
    Ok(())
}

/// Persist the whole edit form through the daemon, which is the single writer
/// of the config file.
async fn save_profile(draft: Draft) -> Result<(), String> {
    let profile = profile_from_draft(&draft)?;

    if draft.exe.trim().is_empty() {
        return Err("可执行文件路径不能为空".to_string());
    }

    let mut params = vec![
        ("id", Value::String(draft.game_id.clone())),
        (
            "profile",
            serde_json::to_value(&profile).map_err(|e| e.to_string())?,
        ),
    ];
    // Only send paths that actually changed: the daemon rejects a path that
    // does not exist, and a game on an unmounted drive must not block a scale
    // edit.
    if draft.game_dir_changed() {
        params.push(("game_dir", Value::String(draft.game_dir.trim().to_string())));
    }
    if draft.exe_changed() {
        params.push(("exe_path", Value::String(draft.exe.trim().to_string())));
    }
    if draft.save_paths_changed() {
        params.push(("save_paths", save_paths_to_json(&draft.save_paths)));
    }

    crate::rpc::call(
        &crate::config::socket_path(),
        "game.update",
        Some(crate::rpc::params(params)),
    )
    .await?;
    Ok(())
}

fn profile_from_draft(draft: &Draft) -> Result<ScaleProfile, String> {
    let algorithm = ScaleAlgorithm::from_label(&draft.algo)
        .ok_or_else(|| format!("未知缩放算法: {}", draft.algo))?
        .with_sharpness(draft.sharpness);

    // An empty field means "no ratio": the stored output size keeps driving the
    // window. A half-typed number is an error the user can fix, not a silent
    // fallback that would throw their ratio away.
    let scale_ratio = match draft.scale_ratio.trim() {
        "" => None,
        raw => Some(
            raw.parse::<f32>()
                .map_err(|_| format!("缩放比例必须是数字（当前 {raw}）"))?,
        ),
    };

    Ok(ScaleProfile {
        name: draft.profile_name.clone(),
        algorithm,
        internal_width: parse_u32(&draft.internal_w, "游戏分辨率宽")?,
        internal_height: parse_u32(&draft.internal_h, "游戏分辨率高")?,
        output_width: parse_u32(&draft.output_w, "输出分辨率宽")?,
        output_height: parse_u32(&draft.output_h, "输出分辨率高")?,
        scale_ratio,
        follow_window: draft.follow_window,
        framerate_limit: if draft.framerate.trim().is_empty() {
            None
        } else {
            Some(parse_u32(&draft.framerate, "帧率限制")?)
        },
        force_fullscreen: draft.fullscreen,
    })
}

fn parse_u32(s: &str, label: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} 必须是正整数"))
}

fn parse_games(value: &Value) -> Result<Vec<UiGame>, String> {
    let games = value
        .get("games")
        .and_then(|g| g.as_array())
        .ok_or_else(|| "守护进程返回格式异常".to_string())?;

    games
        .iter()
        .map(|g| {
            // The daemon sends the whole GameConfig, so read `scale_profile`
            // directly instead of a hand-picked subset.
            let scale = g.get("scale_profile");
            let algorithm = scale.and_then(|s| s.get("algorithm"));

            Ok(UiGame {
                id: g
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: g
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知")
                    .to_string(),
                game_dir: g
                    .get("game_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                save_paths: parse_save_paths(g.get("save_paths")),
                watch_only: g
                    .get("watch_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                process_name: g
                    .get("process_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                exe: g
                    .get("exe_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                profile_name: scale
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("默认")
                    .to_string(),
                algo: algorithm
                    .and_then(algo_label)
                    .unwrap_or_else(|| "Fsr".to_string()),
                sharpness: algorithm.and_then(algo_sharpness).unwrap_or(2),
                internal: (
                    u32_field(scale, "internal_width").unwrap_or(0),
                    u32_field(scale, "internal_height").unwrap_or(0),
                ),
                output: (
                    u32_field(scale, "output_width").unwrap_or(0),
                    u32_field(scale, "output_height").unwrap_or(0),
                ),
                scale_ratio: scale
                    .and_then(|s| s.get("scale_ratio"))
                    .and_then(|v| v.as_f64())
                    .map(|r| r as f32),
                follow_window: scale
                    .and_then(|s| s.get("follow_window"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                fullscreen: scale
                    .and_then(|s| s.get("force_fullscreen"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                framerate: scale
                    .and_then(|s| s.get("framerate_limit"))
                    .and_then(|v| v.as_u64().map(|f| f as u32)),
            })
        })
        .collect()
}

fn u32_field(parent: Option<&Value>, key: &str) -> Option<u32> {
    parent?.get(key)?.as_u64().map(|v| v as u32)
}

/// Algorithm label from the serialized form. Serde tags struct variants as an
/// object (`{"Fsr": {"sharpness": 2}}`) but unit variants as a bare string
/// (`"Integer"`), so both shapes must be handled.
fn algo_label(v: &Value) -> Option<String> {
    if let Some(label) = v.as_str() {
        return Some(label.to_string());
    }
    let obj = v.as_object()?;
    obj.keys().next().cloned()
}

/// `sharpness` of the serialized algorithm, when it has one.
fn algo_sharpness(v: &Value) -> Option<u32> {
    let obj = v.as_object()?;
    let (_k, val) = obj.iter().next()?;
    val.get("sharpness")?.as_u64().map(|s| s as u32)
}

/// Crash report file, next to the daemon log.
const UI_CRASH_LOG: &str = "ui-crash.log";

/// Record a panic before the process dies.
///
/// The GUI lives in a terminal the user closes as soon as something goes
/// wrong, and stderr dies with it — which is how a crash becomes "it just
/// exited for no reason". Keeping the report on disk makes it explainable.
///
/// Returns the path and whether it could actually be written to; the message
/// printed on a crash must not promise a file that is not there.
fn install_crash_log() -> (PathBuf, bool) {
    let dir = crate::config::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(UI_CRASH_LOG);

    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    };
    // Find out now, while there is still someone to tell.
    let writable = open().is_ok();

    let report_path = path.clone();
    let note = if writable {
        format!("kotori 崩溃了，原因已写入 {}", path.display())
    } else {
        format!(
            "kotori 崩溃了：{} 写不进去（目录只读？），报告只在上面这段输出里",
            path.display()
        )
    };
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;

        let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let report = format!(
            "\n===== {when} =====\n{info}\n\nbacktrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&report_path)
        {
            let _ = file.write_all(report.as_bytes());
        }
        eprint!("{report}");
        eprintln!("{note}");
    }));

    (path, writable)
}

pub fn run() -> anyhow::Result<()> {
    // Niri + wgpu Vulkan swapchain constantly reports SurfaceError::Outdated,
    // producing an ERROR log storm (33k lines in 6s). Force GL/EGL by default;
    // an explicit WGPU_BACKEND env from the user still takes precedence.
    if std::env::var_os("WGPU_BACKEND").is_none() {
        unsafe { std::env::set_var("WGPU_BACKEND", "gl") };
    }

    let (crash_log, crash_log_ok) = install_crash_log();
    tracing::info!(
        "UI 渲染后端 {}；崩溃报告 {}",
        std::env::var("WGPU_BACKEND").unwrap_or_else(|_| "自动".into()),
        if crash_log_ok {
            crash_log.display().to_string()
        } else {
            format!("写不进 {}（目录不可写）", crash_log.display())
        }
    );

    let socket = crate::config::socket_path();

    // Make sure the daemon is up; if it cannot be started the UI still opens
    // and simply reports「未连接」.
    if let Err(e) = crate::daemon::ensure_running(&socket) {
        tracing::warn!("{e}");
    }

    iced::application("Kotori", App::update, App::view)
        .font(UI_FONT_BYTES)
        .default_font(ui_font())
        .window(iced::window::Settings {
            size: Size::new(960.0, 640.0),
            min_size: Some(Size::new(760.0, 520.0)),
            ..Default::default()
        })
        .theme(|_| Theme::Dark)
        .run_with(App::new)
        .map_err(|e| anyhow::anyhow!("UI error: {e}"))?;

    // Reaching this line means the event loop ended because every window was
    // closed — not because of a panic. Worth recording: "it just exited" is
    // ambiguous otherwise.
    tracing::info!("UI 退出：所有窗口已关闭（不是崩溃）");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn daemon_game_list() -> Value {
        json!({
            "games": [{
                "id": "demo",
                "name": "demo",
                "exe_path": "/games/demo/game.exe",
                "save_paths": [],
                "wine_prefix": null,
                "created_at": "2026-01-01T00:00:00Z",
                "scale_profile": {
                    "name": "自定义",
                    "algorithm": { "Nis": { "sharpness": 4 } },
                    "internal_width": 1920,
                    "internal_height": 1080,
                    "output_width": 2560,
                    "output_height": 1440,
                    "framerate_limit": 60,
                    "force_fullscreen": false
                }
            }]
        })
    }

    fn draft_with(algo: &str) -> Draft {
        Draft {
            game_id: "x".into(),
            game_name: "x".into(),
            profile_name: "默认".into(),
            game_dir: "/games/x".into(),
            game_dir_original: "/games/x".into(),
            exe: "/games/x/game.exe".into(),
            exe_original: "/games/x/game.exe".into(),
            save_paths: Vec::new(),
            save_paths_original: Vec::new(),
            algo: algo.into(),
            sharpness: 2,
            internal_w: "1280".into(),
            internal_h: "720".into(),
            output_w: "2560".into(),
            output_h: "1440".into(),
            scale_ratio: String::new(),
            follow_window: true,
            fullscreen: true,
            framerate: String::new(),
        }
    }

    #[test]
    fn parses_the_full_scale_profile() {
        let games = parse_games(&daemon_game_list()).unwrap();
        let game = &games[0];
        assert_eq!(game.id, "demo");
        assert_eq!(game.profile_name, "自定义");
        assert_eq!(game.algo, "Nis");
        assert_eq!(game.sharpness, 4);
        assert_eq!(game.internal, (1920, 1080));
        assert_eq!(game.output, (2560, 1440));
        assert_eq!(game.framerate, Some(60));
        assert!(!game.fullscreen);
    }

    #[test]
    fn open_then_save_preserves_stored_values() {
        // Regression test for the silent overwrite: the draft used to start
        // from hard-coded defaults (sharpness 2 / fullscreen true / no fps).
        let game = parse_games(&daemon_game_list()).unwrap().remove(0);
        let draft = Draft::from_game(&game);

        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.name, "自定义");
        assert_eq!(profile.algorithm, ScaleAlgorithm::Nis { sharpness: 4 });
        assert_eq!(profile.framerate_limit, Some(60));
        assert!(!profile.force_fullscreen);
    }

    #[test]
    fn unit_variant_algorithms_survive_a_load_save_cycle() {
        // serde writes unit variants as a bare string (`"Integer"`). Reading
        // that as an object used to silently turn the game into FSR on save.
        let value = json!({
            "games": [{
                "id": "int",
                "name": "int",
                "exe_path": "/int.exe",
                "scale_profile": {
                    "name": "默认",
                    "algorithm": "Integer",
                    "internal_width": 640,
                    "internal_height": 480,
                    "output_width": 1280,
                    "output_height": 960
                }
            }]
        });
        let game = parse_games(&value).unwrap().remove(0);
        assert_eq!(game.algo, "Integer");

        let draft = Draft::from_game(&game);
        assert_eq!(draft.algo, "Integer");
        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.algorithm, ScaleAlgorithm::Integer);
        assert_eq!(profile.internal_width, 640);
        assert_eq!(profile.output_width, 1280);
    }

    #[test]
    fn malformed_and_empty_response_are_errors() {
        assert!(parse_games(&json!({})).is_err());
        assert!(parse_games(&json!({ "games": [] })).unwrap().is_empty());
    }

    #[test]
    fn missing_optional_fields_fall_back_safely() {
        let value = json!({
            "games": [{
                "id": "x",
                "name": "x",
                "exe_path": "/x.exe",
                "scale_profile": { "algorithm": "Integer" }
            }]
        });
        let games = parse_games(&value).unwrap();
        assert_eq!(games[0].algo, "Integer");
        assert_eq!(games[0].sharpness, 2);
        assert_eq!(games[0].internal, (0, 0));
        assert!(!games[0].fullscreen);
    }

    #[test]
    fn unknown_algorithm_is_rejected_on_save() {
        assert!(profile_from_draft(&draft_with("Lanczos")).is_err());
    }

    #[test]
    fn non_numeric_resolution_is_rejected() {
        let mut draft = draft_with("Fsr");
        draft.internal_w = "abc".into();
        let err = profile_from_draft(&draft).unwrap_err();
        assert!(err.contains("游戏分辨率宽"), "{err}");
    }

    /// A plain open + save must neither invent nor drop a scaling ratio:
    /// "no ratio" (the state every older profile is in) stays "no ratio", a set
    /// one survives, and half-typed text is an error rather than a silent reset.
    #[test]
    fn the_scaling_ratio_survives_an_open_and_save() {
        let game = ui_game();
        let untouched = profile_from_draft(&Draft::from_game(&game)).unwrap();
        assert_eq!(untouched.scale_ratio, None);
        assert!(untouched.follow_window);

        let mut pinned = game.clone();
        pinned.scale_ratio = Some(1.5);
        pinned.follow_window = false;
        let profile = profile_from_draft(&Draft::from_game(&pinned)).unwrap();
        assert_eq!(profile.scale_ratio, Some(1.5));
        assert!(!profile.follow_window);

        let mut half_typed = Draft::from_game(&pinned);
        half_typed.scale_ratio = "1.5x".into();
        let err = profile_from_draft(&half_typed).unwrap_err();
        assert!(err.contains("缩放比例"), "{err}");
    }

    /// A freshly parsed game must not look "edited" to the save button.
    impl UiGame {
        fn save_paths_changed_after_edit(&self) -> bool {
            Draft::from_game(self).save_paths_changed()
        }
    }

    fn ui_game() -> UiGame {
        UiGame {
            id: "demo".into(),
            name: "Demo Game".into(),
            game_dir: "/games/demo".into(),
            exe: "/games/demo/game.exe".into(),
            save_paths: Vec::new(),
            watch_only: false,
            process_name: String::new(),
            profile_name: "默认".into(),
            algo: "Fsr".into(),
            sharpness: 2,
            internal: (1280, 720),
            output: (2560, 1440),
            scale_ratio: None,
            follow_window: true,
            fullscreen: true,
            framerate: None,
        }
    }

    #[test]
    fn search_matches_name_and_path_case_insensitively() {
        let game = ui_game();
        assert!(matches_query(&game, ""), "empty query shows everything");
        assert!(matches_query(&game, "   "));
        assert!(matches_query(&game, "demo"));
        assert!(matches_query(&game, "DEMO"));
        assert!(matches_query(&game, "Game")); // name
        assert!(matches_query(&game, "games/demo")); // path
        assert!(matches_query(&game, ".exe"));
        assert!(!matches_query(&game, "nonexistent"));
    }

    #[test]
    fn retry_backoff_grows_then_caps() {
        assert_eq!(retry_delay(1), std::time::Duration::from_secs(2));
        assert_eq!(retry_delay(2), std::time::Duration::from_secs(4));
        assert_eq!(retry_delay(3), std::time::Duration::from_secs(8));
        assert_eq!(retry_delay(4), std::time::Duration::from_secs(16));
        // capped, and never overflows for a large attempt count
        assert_eq!(retry_delay(5), std::time::Duration::from_secs(16));
        assert_eq!(retry_delay(99), std::time::Duration::from_secs(16));
    }

    #[test]
    fn parses_wine_status() {
        let value = json!({
            "configured": "/prefixes/games",
            "default": "/home/user/.wine",
            "environment": null,
            "detected": ["/home/user/.local/share/wineprefixes/a", "/home/user/.wine"]
        });
        let status = parse_wine_status(&value);
        assert_eq!(status.configured.as_deref(), Some("/prefixes/games"));
        assert_eq!(status.default_prefix, "/home/user/.wine");
        assert_eq!(status.environment, None);
        assert_eq!(status.detected.len(), 2);

        // A daemon that reports nothing usable still yields a sane value.
        let empty = parse_wine_status(&json!({}));
        assert_eq!(empty.configured, None);
        assert!(empty.detected.is_empty());
    }

    #[test]
    fn save_paths_round_trip_between_editor_and_daemon() {
        let paths = vec![
            SavePathDraft {
                kind: "windows".into(),
                path: "%APPDATA%\\Game".into(),
                exclude: "*.log, cache/".into(),
            },
            SavePathDraft {
                kind: "relative".into(),
                path: "savedata".into(),
                exclude: String::new(),
            },
        ];

        let json = save_paths_to_json(&paths);
        assert_eq!(json[0]["kind"], "windows");
        assert_eq!(json[0]["exclude"][0], "*.log");
        assert_eq!(json[0]["exclude"][1], "cache/");
        assert!(
            json[1].get("exclude").is_none(),
            "an empty exclude list must not be sent"
        );

        assert_eq!(
            parse_save_paths(Some(&json)),
            paths,
            "editor -> daemon -> editor must be lossless"
        );
        assert!(parse_save_paths(None).is_empty());
    }

    #[test]
    fn kind_placeholders_teach_each_format() {
        assert_eq!(kind_placeholder("windows"), "%APPDATA%\\Game\\save");
        assert!(kind_placeholder("relative").contains("save"));
        assert!(kind_placeholder("absolute").starts_with('/'));
    }

    #[test]
    fn parses_watch_mode_and_save_paths_from_the_daemon() {
        let value = json!({
            "games": [{
                "id": "w",
                "name": "W",
                "game_dir": "/games/w",
                "exe_path": "/games/w/game.exe",
                "watch_only": true,
                "process_name": "game.exe",
                "save_paths": [
                    { "kind": "windows", "path": "%APPDATA%\\W", "exclude": ["*.log", "tmp/"] }
                ],
                "scale_profile": { "algorithm": "Integer" }
            }]
        });

        let game = parse_games(&value).unwrap().remove(0);
        assert!(game.watch_only);
        assert_eq!(game.process_name, "game.exe");
        assert_eq!(game.save_paths.len(), 1);
        assert_eq!(game.save_paths[0].kind, "windows");
        assert_eq!(game.save_paths[0].exclude, "*.log, tmp/");
        assert!(!game.save_paths_changed_after_edit());
    }

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

    /// Building the widget tree must not panic in any reachable state. This
    /// covers the empty-list / no-search-hit branches of the new pages.
    /// A `sync.status` payload as the daemon sends it.
    fn sync_payload() -> Value {
        serde_json::json!({
            "settings": {
                "enabled": true,
                "endpoint": "",
                "bucket": "kotori-saves",
                "prefix": "kotori",
                "encryption": false,
                "keep_versions": 0
            },
            "enabled": true,
            "rclone": "/usr/bin/rclone",
            "keyring": {
                "backend": "Secret Service (libsecret) (/usr/bin/secret-tool)",
                "ephemeral": false,
                "store": { "kind": "system", "backend": "Secret Service (libsecret)" },
                "secrets_file": "/home/user/.config/kotori/secrets.json",
                "min_master_password": 8
            },
            "secrets": ["b2-key-id", "b2-app-key", "sync-password"],
            "ready": true,
            "problem": null,
            "remote": "kotori:kotori-saves/kotori",
            "password_hint": "secret-tool lookup service kotori account sync-password",
            "games": [
                {
                    "id": "demo",
                    "name": "Demo",
                    "locations": 2,
                    "location_problem": null,
                    "last": {
                        "at": "2026-09-11T10:15:00Z",
                        "ok": true,
                        "action": "上传",
                        "detail": "2 个位置已上传"
                    }
                },
                {
                    "id": "other",
                    "name": "Other",
                    "locations": 0,
                    "location_problem": null,
                    "last": null
                }
            ]
        })
    }

    fn sync_status_fixture() -> SyncStatus {
        parse_sync_status(&sync_payload()).unwrap()
    }

    #[test]
    fn parses_the_sync_status() {
        let status = sync_status_fixture();
        assert!(status.ready);
        assert!(!status.ephemeral);
        assert_eq!(status.store_kind, "system");
        assert!(!status.store_locked);
        assert_eq!(status.min_master_password, 8);
        assert_eq!(status.remote, "kotori:kotori-saves/kotori");
        assert_eq!(status.rclone.as_deref(), Some("/usr/bin/rclone"));
        assert!(status.problem.is_none());
        assert!(status.keyring.contains("Secret Service"));
        assert!(
            status.password_hint.contains("secret-tool"),
            "the user must be able to read the password back without kotori"
        );

        assert_eq!(status.games.len(), 2);
        assert_eq!(status.games[0].locations, 2);
        let last = status.games[0].last_label();
        assert!(last.contains("✓") && last.contains("上传"), "{last}");
        assert_eq!(status.games[1].last_label(), "还没同步过");

        // A malformed payload is an error, not a silently empty page.
        assert!(parse_sync_status(&Value::Null).is_err());
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
    fn the_credentials_section_counts_what_is_stored() {
        // One pair of keys per account, so "how many" means "how many of the
        // two halves are there" — the values never come back from the daemon.
        assert!(credentials_label(true, true).contains("2/2"));
        assert!(credentials_label(true, false).contains("1/2"));
        let empty = credentials_label(false, false);
        assert!(empty.contains("0/2"), "{empty}");
        assert!(empty.contains("未保存"), "{empty}");
        assert!(empty.contains("覆盖"), "覆盖语义要写出来：{empty}");

        let status = sync_status_fixture();
        assert!(status.has_secret("b2-key-id") && status.has_secret("sync-password"));
        assert!(!status.has_secret("b2-app-key-x"));
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

    #[test]
    fn sync_outcomes_are_summarised_for_a_human() {
        // A bulk upload: one location moved, one skipped.
        let value = serde_json::json!({
            "ok": true,
            "games": [{
                "game_id": "demo",
                "name": "Demo",
                "ok": true,
                "locations": [
                    { "configured": "savedata", "local": "/g/savedata", "action": "uploaded", "detail": "已上传" },
                    { "configured": "%APPDATA%\\\\X", "local": "/w/X", "action": "skipped", "detail": "本地没有这个目录" }
                ]
            }]
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("1 个位置已同步"), "{summary}");
        assert!(summary.contains("跳过 1"), "{summary}");

        // Nothing changed is not a failure.
        let value = serde_json::json!({
            "ok": true,
            "games": [{ "game_id": "demo", "name": "Demo", "ok": true, "locations": [] }]
        });
        assert!(describe_sync_outcome(&value).contains("没有需要同步的变化"));

        // A failure names the location that broke.
        let value = serde_json::json!({
            "ok": false,
            "game": {
                "game_id": "demo",
                "name": "Demo",
                "ok": false,
                "error": "savedata: rclone 执行失败",
                "locations": []
            }
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("失败"), "{summary}");
        assert!(summary.contains("savedata"), "{summary}");
    }

    #[test]
    fn views_construct_for_every_tab_and_state() {
        let (mut app, _task) = App::new();

        // Library: empty, populated, filtered, no match.
        app.tab = Tab::Games;
        let _ = app.view();
        app.games = vec![ui_game()];
        let _ = app.view();

        // A watch-only card, with and without a process name.
        app.games = vec![UiGame {
            watch_only: true,
            process_name: "game.exe".into(),
            ..ui_game()
        }];
        let _ = app.view();
        app.games = vec![ui_game()];

        // Live sessions (running vs. monitoring) change the action button.
        app.running.insert(
            "demo".into(),
            SessionInfo {
                session_id: "session-1".into(),
                watch_only: false,
            },
        );
        let _ = app.view();
        app.running.insert(
            "demo".into(),
            SessionInfo {
                session_id: "session-1".into(),
                watch_only: true,
            },
        );
        let _ = app.view();
        app.running.clear();

        app.search = "demo".into();
        let _ = app.view();
        app.search = "zzz".into();
        let _ = app.view();
        app.search.clear();

        // Detail page, with and without the delete confirmation.
        app.selected = Some("demo".into());
        app.draft = Some(Draft::from_game(&ui_game()));
        let _ = app.view();

        // With save locations in the editor (one of each kind).
        app.draft = Some(Draft {
            save_paths: vec![
                SavePathDraft {
                    kind: "windows".into(),
                    path: "%APPDATA%\\Game".into(),
                    exclude: "*.log".into(),
                },
                SavePathDraft {
                    kind: "relative".into(),
                    path: "savedata".into(),
                    exclude: String::new(),
                },
                SavePathDraft {
                    kind: "absolute".into(),
                    path: "/saves/demo".into(),
                    exclude: String::new(),
                },
            ],
            ..Draft::from_game(&ui_game())
        });
        let _ = app.view();

        app.confirm_delete = true;
        let _ = app.view();

        // Add page: empty form, filled form, with and without a message.
        app.tab = Tab::Add;
        app.selected = None;
        app.draft = None;
        app.confirm_delete = false;
        let _ = app.view();
        app.new_name = "Demo".into();
        app.new_game_dir = "/games/demo".into();
        app.new_exe = "/games/demo/game.exe".into();
        let _ = app.view();
        app.create_msg = Some("已添加（ID: demo）".into());
        let _ = app.view();

        // Settings page: before and after the wine status arrives.
        app.tab = Tab::Settings;
        app.wine_status = None;
        let _ = app.view();
        app.wine_status = Some(WineStatus {
            configured: Some("/prefixes/games".into()),
            default_prefix: "/home/user/.wine".into(),
            environment: None,
            detected: vec!["/home/user/.wine".into()],
        });
        app.wine_msg = Some("已保存".into());
        let _ = app.view();

        // Sync section: nothing loaded yet, then a ready setup, then the two
        // states that ask for a decision (a restore and an encryption flip).
        app.sync_status = None;
        app.sync_form = SyncForm::default();
        let _ = app.view();

        app.sync_status = Some(sync_status_fixture());
        app.sync_form.apply(
            app.sync_status.as_ref().unwrap(),
            &sync_payload()["settings"],
        );
        let _ = app.view();

        app.sync_form.confirm_encryption = Some(true);
        let _ = app.view();
        app.sync_form.confirm_encryption = None;

        app.sync_restore_pending = Some(("demo".into(), None));
        let _ = app.view();
        app.sync_restore_pending = None;

        // A locked credential file: the page must offer "unlock", not
        // "enter your B2 keys again".
        app.sync_status = Some(SyncStatus {
            store_kind: "encrypted-file".into(),
            store_locked: true,
            store_path: "/home/user/.config/kotori/secrets.json".into(),
            keyring: "主密码加密文件 /home/user/.config/kotori/secrets.json（已锁定）".into(),
            secrets: Vec::new(),
            ready: false,
            problem: Some("凭据文件已锁定，请先用主密码解锁".into()),
            ..sync_status_fixture()
        });
        app.sync_form.master_password = "typed".into();
        let _ = app.view();

        // ...and once unlocked, no password field at all.
        app.sync_status = Some(SyncStatus {
            store_locked: false,
            secrets: vec!["b2-key-id".into(), "b2-app-key".into()],
            ready: true,
            problem: None,
            ..app.sync_status.clone().unwrap()
        });
        let _ = app.view();

        // A machine with no keyring at all: explain, and offer the way out.
        app.sync_status = Some(SyncStatus {
            store_kind: "session-only".into(),
            store_locked: false,
            keyring: "内存（本机没有运行中的系统密钥环，重启后需要重新输入）".into(),
            ephemeral: true,
            ..sync_status_fixture()
        });
        let _ = app.view();
        app.sync_status = Some(sync_status_fixture());
        app.sync_form.master_password.clear();

        // A machine with no keyring, no rclone and an unresolvable save path.
        app.sync_status = Some(SyncStatus {
            rclone: None,
            ephemeral: true,
            keyring: "内存（没有系统密钥环，重启后需重新输入）".into(),
            problem: Some("密钥环里还没有 B2 凭据".into()),
            games: vec![SyncGameRow {
                id: "demo".into(),
                name: "Demo".into(),
                locations: 0,
                problem: Some("存档位置「%NOPE%」解析不了".into()),
                last: Some("✗ 2026-09-11T10:15 ✓".into()),
            }],
            ..sync_status_fixture()
        });
        let _ = app.view();

        // Settings, plus the disconnected sidebar with its reconnect button.
        app.tab = Tab::Settings;
        for (connected, attempts) in [
            (Some(true), 0),
            (Some(false), 1),
            (Some(false), 99),
            (None, 0),
        ] {
            app.daemon_connected = connected;
            app.retry_attempts = attempts;
            app.error = Some("boom".into());
            let _ = app.view();
        }
    }
}

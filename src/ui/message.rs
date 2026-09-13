//! The Elm-style message enum and the small enums it carries.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Games,
    Add,
    /// 云同步独占一页(旧 UI 把它塞在「设置」里,Win11 的导航更适合分开)。
    Sync,
    Settings,
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
    /// 全局快捷键的注册情况(`daemon.status.hotkeys`),设置页要看它。
    /// 独立于 `StatusLoaded`:那是每 3 秒一次的会话轮询,这条只在打开设置页时问一次。
    HotkeysLoaded(Result<HotkeyStatus, String>),
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
    /// 锁上主密码凭据文件(密钥只留在内存里),以及删掉它(里面的凭据一起消失)。
    SyncLockCredentials,
    SyncCredentialsLocked(Result<(), String>),
    SyncMasterDeleteRequested,
    SyncMasterDeleteCancelled,
    SyncMasterDeleteConfirmed,
    SyncMasterDeleted(Result<(), String>),
    SyncNow(Option<String>),
    SyncNowDone(Result<String, String>),
    SyncRestoreRequested(String, Option<String>),
    SyncRestoreCancelled,
    SyncRestoreConfirmed,
}

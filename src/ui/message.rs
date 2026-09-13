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

/// 「浏览…」要往哪个输入框里填。
///
/// 一个枚举而不是"页面上第几个框":回调是各页各自声明的(`wire.rs` 里一对一映射),
/// 所以这里多一点名字换来的是"改一个页面不会串到另一个页面"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathTarget {
    /// 添加游戏页的两个框。
    NewGameDir,
    NewExe,
    /// 单个游戏设置页:路径那一组的两个框。
    GameDir,
    Exe,
    /// 单个游戏设置页:第几行存档位置。
    SavePath(usize),
    /// 设置页:wine prefix。
    WinePrefix,
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
    /// 改一下就自动存一次:这是防抖定时器到点。
    ///
    /// 带着世代号 —— 定时器醒来时它已经过期(用户还在改)就什么都不做,见
    /// `App::schedule_auto_save`。没有「保存」按钮了,所以这条是唯一的写入入口。
    AutoSave(u64),
    /// 一次自动保存的回包。世代号对不上说明这一笔已经过期(用户按过「重置」或又改了),
    /// 那时得拿手上的草稿再存一次,否则配置里留着的是一个用户已经不要的值。
    ProfileSaved(u64, Result<(), String>),
    /// 「重置」:回到已保存的设置(没有保存按钮之后,这是填错值的唯一退路)。
    ResetProfile,
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

    // ── 「浏览…」:借系统自己的对话框挑一个位置 ──────────────────────────────
    /// 这台机器上有没有可用的对话框(没有就给出理由)。开机问一次。
    PickerProbed(Result<(), String>),
    /// 用户点了「浏览…」。
    PickPath(PathTarget),
    /// 对话框回来了:`Ok(None)` 是用户取消,`Err` 是它根本打不开。
    PathPicked(PathTarget, Result<Option<std::path::PathBuf>, String>),
    /// 设置页的「后台服务」:手动把守护进程拉起来 / 停掉(以前只能敲命令行)。
    ServiceStart,
    ServiceStarted(Result<String, String>),
    ServiceStop,
    ServiceStopped(Result<String, String>),
    StatusLoaded(Result<std::collections::BTreeMap<String, SessionInfo>, String>),
    /// 设置页「环境检查」的结果(`env.report`),以及用户按下的「重新检查」。
    EnvironmentLoaded(Result<Environment, String>),
    EnvironmentReload,
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

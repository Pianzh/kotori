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
    /// 两个引擎的可执行文件在哪（云同步页的「程序位置」）。
    RcloneBinary,
    KopiaBinary,
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
    /// 云同步页:两个引擎的可执行文件位置(「程序位置」那一组)。
    RcloneBinary,
    KopiaBinary,
}

#[derive(Debug, Clone)]
pub enum Message {
    TabChanged(Tab),
    Refresh,
    GamesLoaded(Result<Vec<UiGame>, String>),
    /// 那一颗「启动 / 停止」按钮(同一颗按钮两种时候):该启动还是该停,由 Rust 看
    /// 会话表决定(见 `App::run_action`)—— 文案与动作出自同一个判断。
    ToggleRun(String),
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
    DirectLaunchToggled(bool),
    AutoWatchToggled(bool),
    /// 详情页「跟随的进程」那一栏(存进配置的进程名)。
    ProcessNameChanged(String),
    /// 详情页「跟这一局」那一栏里那个 pid 框(只对这一次运行有意义,不进草稿)。
    FollowPidChanged(String),
    /// 点「跟这一局」。
    FollowThisRun,
    FollowDone(Result<String, String>),
    /// 打开「从运行中的进程里挑」浮层(两个入口共用,差别在 `PickerPurpose`)。
    ProcessPickerOpen(PickerPurpose),
    /// 候选到了(打开时那一次 `process.list`)。
    ProcessesLoaded(Result<Vec<ProcessRow>, String>),
    ProcessQueryChanged(String),
    /// 挑了**过滤后**那一份里的第 index 行。
    ProcessPicked(usize),
    ProcessPickerClose,
    FullscreenToggled(bool),
    FramerateChanged(String),
    ExePathChanged(String),
    /// exe 的额外参数。页面上是一行文本,存的时候按空白切成 argv
    /// (见 `parse::scale::split_args`)。
    LaunchArgsChanged(String),
    /// 手写的 gamescope 参数(切分规则同上)。它非空时档案里其它缩放设置**全部
    /// 让位**,运行时缩放也一并停用,见 `ScaleProfile::gamescope_args`。
    GamescopeArgsChanged(String),
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
    CreateFinished(Result<(String, Option<String>), String>),
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
    StatusLoaded(Result<DaemonStatus, String>),
    /// 设置页:把配置切到便携 / 默认地点(`true` = 便携)。
    ConfigSourcePicked(bool),
    ConfigSourceSwitched(Result<String, String>),
    /// 设置页「环境检查」的结果(`env.report`),以及用户按下的「重新检查」。
    EnvironmentLoaded(Result<Environment, String>),
    EnvironmentReload,
    StopDone(Result<(), String>),
    Tick,

    // --- cloud sync (settings tab) ---
    /// Box 起来的：`SyncStatus` 是这里最大的一份状态（三个引擎相关的字符串加上
    /// 游戏列表），直接塞进枚举会把每一个 `Message` 都撑到几百字节 —— 而消息在
    /// 每条事件上都要移动一次。
    SyncStatusLoaded(Box<Result<SyncStatus, String>>),
    SyncToggleEnabled(bool),
    SyncField(SyncField, String),
    /// 换引擎（`rclone` / `kopia`）。只改表单，随"保存设置"一起提交。
    SyncEngineSelected(String),
    /// 引擎那一次提交的回包。`Ok(true)` = 这次**真的换掉了**引擎，界面要把
    /// daemon 那条警告说出来（换引擎之后另一个引擎传的版本读不出来，且不报错）。
    ///
    /// 它和 [`Message::SyncSettingsSaved`] 分开，是因为两者对
    /// `sync_form.settings_dirty` 的处理相反：换引擎只提交了 engine 一个字段，
    /// 用户手上那些还没保存的编辑一个字都没动，不能被当成"已保存"。
    SyncEngineSaved(Result<bool, String>),
    /// kopia 仓库密码：它不是 `[sync]` 里的设置项，所以不走 [`SyncField`]。
    SyncKopiaPasswordChanged(String),
    SyncSaveKopiaPassword,
    /// `Ok(true)` = 清除成功，回到默认密码。
    SyncKopiaPasswordSaved(Result<bool, String>),
    SyncSaveSettings,
    /// `Ok(true)` = 这一笔里有引擎变更，那条"对面数据看不见"的警告要说出来。
    SyncSettingsSaved(Result<bool, String>),
    SyncSaveCredentials,
    SyncCredentialsSaved(Result<(), String>),
    SyncClearCredentials,
    SyncCredentialsCleared(Result<(), String>),
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

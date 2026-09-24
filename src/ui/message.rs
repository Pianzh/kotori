//! The Elm-style message enum and the small enums it carries.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Games,
    Add,
    /// 「云端存档」：云端有哪些游戏、点开看每一版（只读云端）。
    Cloud,
    /// 云同步独占一页(旧 UI 把它塞在「设置」里,Win11 的导航更适合分开)。
    ///
    /// ⚠ 用户 2026-09-23 定了：这一页最后要**整页搬进「设置」**（导航只剩
    /// 游戏库 / 添加 / 云端存档 / 设置），那一步与配对表的合并一起做。
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
    /// 「停止」的二次确认里用户按了取消(那颗按钮会真的把游戏结束掉,所以问一次)。
    StopCancelled,
    LaunchDone(Result<Value, String>),
    /// 启动前那一问的回答：`ok`（没问题）/ `pair`（新建一条身份）/ `off`（关掉这一款的
    /// 同步）。三个回答之后都会照常启动，见 `widgets/sync-ask.slint`。
    SyncAskAnswered(String),
    /// 「改配对…」：收起那一问、打开云端清单，从云端已有的一条里挑（见 `model::cloud_pick`）。
    SyncAskPairRequested,
    /// 单游戏页那颗「参与云同步」开关（`sync_enabled`，用户 2026-09-24 要的界面入口）。
    SyncParticipatingToggled(bool),
    SyncParticipatingSaved(bool, Result<(), String>),
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
    /// 打开「从运行中的进程里挑」浮层(添加游戏页那个入口:挑一个正在跑的进程,
    /// 把它变成一条新档案)。
    ProcessPickerOpen,
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
    /// 添加页的云端匹配:防抖到点(`MatchExeReady` 带回当时那个 exe,已经不是当前值就丢掉)、
    /// 回包、挑一条、「不是这一款」、改主意(见 `model::add`)。
    MatchExeReady(String),
    MatchLoaded(String, Result<(bool, Vec<CloudGameRow>), String>),
    MatchChoose(String),
    MatchDecline,
    MatchUndoDecline,
    /// 「自己选…」那个浮层:打开(顺带读一次云端清单)、回包、搜索、挑一条、收起、
    /// 以及「改回自动」(见 `model::cloud_pick`)。
    ///
    /// 打开时带上**用途**：添加页挑中的是"这一款就绑它"，启动前那一问挑中的是
    /// "这一次就用它启动" —— 同一个选择器，落点不同。
    CloudPickOpen(CloudPickPurpose),
    CloudPickLoaded(Result<CloudListReply, String>),
    CloudPickSearch(String),
    CloudPickChoose(String),
    CloudPickDismiss,
    MatchClearPick,
    /// 添加之后顺手认领云端那一条的结果(`sync.pair`)。失败**不算添加失败**。
    GamePaired(Result<(), String>),
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
    /// 「云端存档」页：读**索引**列云端有哪些游戏（用户按刷新才走，一次读）。
    CloudRefresh,
    CloudLoaded(Result<CloudListReply, String>),
    /// 深度扫描：读**所有身份卡**、重建索引、顺手把能自动绑的绑上（慢，用户主动按）。
    CloudScan,
    CloudScanned(Result<CloudListReply, String>),
    /// 搜索框变了（本地过滤，不打网络）。
    CloudSearch(String),
    /// 点开 / 收起某一款：参数是**云端落点**。
    CloudToggle(String),
    /// 返回列表。
    CloudBack,
    /// `(云端落点, 这一款的版本明细)`。
    CloudVersionsLoaded(String, Result<Vec<CloudVersionRow>, String>),
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

//! The application state itself, plus the parts of `impl App` that are neither
//! the message loop (`super::update`) nor the view tree (`super::view`).

use super::*;

pub struct App {
    pub(super) tab: Tab,
    pub(super) games: Vec<UiGame>,
    /// 「云端存档」那一块（云端有哪几款、每款几版、点开的那一款有哪几版）。
    pub(super) cloud: CloudState,
    pub(super) daemon_socket: PathBuf,
    pub(super) daemon_connected: Option<bool>,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) launching: Option<String>,
    /// 启动前那一问正等着回答的那一款（`None` = 没在问）。
    pub(super) sync_ask: Option<String>,
    /// 那一问被「改配对…」让位给云端清单时置起：`sync_ask` 还留着（挑完要用它启动），
    /// 但浮层先收起来 —— 两块浮层叠在一起会互相挡住。
    pub(super) sync_ask_hidden: bool,
    /// 单游戏页正等着确认"新建一条云端身份"（两段式：先点、再确认才真的新建）。
    pub(super) sync_new_pending: bool,
    /// 启动前那一问里"疑似找到的那一条"（`None` = 完全没找到，界面照实说）。
    pub(super) sync_ask_cloud: Option<SyncAskCloud>,
    pub(super) selected: Option<String>,
    pub(super) draft: Option<Draft>,
    pub(super) saving: bool,
    pub(super) saved_msg: Option<String>,
    /// 那行小字是好事还是坏事(`saved_msg` 光有文字说明不了 —— "猜前缀"那种做法在
    /// 「浏览…」失败这种新消息上就会显示成绿色)。
    pub(super) saved_ok: bool,
    /// 自动保存用世代号:每改一笔 +1 并挂一个防抖定时器,定时器醒来发现号变了就作废
    /// (说明用户还在改)。没有「保存」按钮之后,这一对(世代 + [`SaveAttempt`])是
    /// 唯一防止"打一个字写一次""旧回包盖掉新内容"的东西,别省。
    pub(super) autosave_generation: u64,
    /// 正在路上的那一笔。**同一时刻只允许一笔**:两次全量写并发时,后到的旧快照会把
    /// 新的盖掉(daemon 是并发的),所以第二笔一律等第一笔回来再发。
    pub(super) save_in_flight: Option<SaveAttempt>,
    /// Library search query (matches name or exe path).
    pub(super) search: String,
    pub(super) confirm_delete: bool,
    /// "Add game" tab state (manual entry — no scanning).
    pub(super) new_name: String,
    pub(super) new_game_dir: String,
    pub(super) new_exe: String,
    /// 添加页那两栏挂载引用（盘号 + 磁盘内相对目录）。挑完 exe / 根目录之后由 daemon
    /// 认一次盘、自动填好（用户 2026-09-25 定的"中间加一小步"）；盘号空 = 不用引用。
    pub(super) new_game_dir_disk: String,
    pub(super) new_game_dir_relative: String,
    pub(super) new_exe_disk: String,
    pub(super) new_exe_relative: String,
    /// exe 联动**自动填过**的根目录与名字:字段为空、或者还等于上次自动填的值时,
    /// 才跟着新 exe 重填 —— 用户自己改过的值不动(用户 2026-09-19:"如果不对再由
    /// 用户自行修改")。
    pub(super) auto_filled_dir: String,
    pub(super) auto_filled_name: String,
    pub(super) creating: bool,
    pub(super) create_msg: Option<String>,
    /// 添加页那一块云端匹配:填完 exe 问一次"云端有没有这一款"(见 `model::add`)。
    /// **它只决定"要不要顺手绑上"**,一个字都不影响这一条能不能建起来。
    pub(super) add_match: AddMatch,
    /// 那块匹配旁边的「自己选…」浮层:从云端清单里挑一条绑上(见 `model::cloud_pick`)。
    pub(super) cloud_pick: CloudPick,
    /// Settings tab: wine prefix.
    pub(super) wine_prefix_input: String,
    /// Set when the user edits the prefix by hand, so a `wine.status` reply that
    /// was already in flight cannot overwrite it.
    pub(super) wine_prefix_dirty: bool,
    pub(super) wine_status: Option<WineStatus>,
    pub(super) wine_msg: Option<String>,
    /// 设置页:后台服务(守护进程)的手动启动 / 停止。
    ///
    /// `daemon_paused` 是用户**亲手**停掉服务之后立的旗:自动重连(每 3 秒的轮询、
    /// 刷新、失败退避重试)这时都不许把守护进程悄悄拉回来 —— 否则"停止"按下去三秒
    /// 就自己复活了。它只在连上守护进程(用户点了启动、或从别处起了一个)时清掉。
    pub(super) service_busy: bool,
    pub(super) service_msg: Option<String>,
    pub(super) daemon_paused: bool,
    /// 「浏览…」:这台机器上有没有可用的系统对话框。`None` = 还没探完(按钮先灰着,
    /// 探完立刻放行 —— 探测是一次 D-Bus 调用,开机那一瞬间就回来了)。
    ///
    /// `Err` 里是**给用户看**的理由:没有 xdg-desktop-portal 的桌面上按钮会一直灰着,
    /// 而灰按钮必须说明为什么,否则用户只会以为坏了(用户 2026-09-13:"没有就不能用")。
    pub(super) picker: Option<Result<(), String>>,
    /// 对话框正开着(挡住第二次点击弹出第二个框)。
    pub(super) picking: bool,
    /// 刚选回来的值,等着被推回**页面自己那份副本**(单游戏设置页的输入框由页面持有,
    /// Rust 平时不往里写 —— 见 `game-settings.slint` 的文件头与 `render/detail.rs`)。
    pub(super) picked_path: Option<(PathTarget, String)>,
    /// 推给页面的"第几次选择"令牌:值一样也要能触发一次(见 `types.slint` 的 `PathPick`)。
    pub(super) pick_token: i32,
    /// 设置页「环境检查」的结果;`None` = 还没问过。
    pub(super) environment: Option<Environment>,
    /// 「从正在运行的进程里挑」:浮层在窗口根上,状态在这儿(入口只有添加游戏页)。
    /// ⚠ 名字带 `process_`,因为 `picker` 已经是「浏览…」那个文件对话框了。
    pub(super) process_picker: ProcessPicker,
    /// 详情页那颗「停止」等二次确认(它会真的把游戏结束掉)。与「删除条目」同一套
    /// 两步样式:**只有真会杀进程的那一局**才置它(观测会话点一下就停,不必吓人)。
    pub(super) confirm_stop: bool,
    /// 设置页「配置文件」:daemon 报的落点与"能不能换"。
    pub(super) config_source: ConfigSource,
    /// 正在切(挡住连点两次)。
    pub(super) config_switching: bool,
    /// 切换的结果:一句话 + 是好消息还是坏消息。
    pub(super) config_msg: Option<(String, bool)>,
    /// Automatic reconnect bookkeeping.
    pub(super) retry_attempts: u32,
    /// Live sessions by game id (refreshed periodically).
    pub(super) running: std::collections::BTreeMap<String, SessionInfo>,
    /// Settings tab: cloud sync.
    pub(super) sync_status: Option<SyncStatus>,
    pub(super) sync_form: SyncForm,
    /// Restore waiting for a second click: (game id, snapshot).
    pub(super) sync_restore_pending: Option<(String, Option<String>)>,
    /// 单游戏页那一页「这一款的云端存档」：云端有哪几版 + 替换的二次确认
    /// （`model::versions`）。它跟着 `game-open` 走，所以不是"另一页"而是一层覆盖。
    pub(super) versions: VersionsState,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let socket = crate::config::socket_path();

        (
            Self {
                tab: Tab::Games,
                games: Vec::new(),
                cloud: CloudState::default(),
                sync_ask: None,
                sync_ask_hidden: false,
                sync_new_pending: false,
                sync_ask_cloud: None,
                daemon_socket: socket,
                daemon_connected: None,
                loading: false,
                error: None,
                launching: None,
                selected: None,
                draft: None,
                saving: false,
                saved_msg: None,
                saved_ok: true,
                autosave_generation: 0,
                save_in_flight: None,
                search: String::new(),
                confirm_delete: false,
                new_name: String::new(),
                new_game_dir: String::new(),
                new_exe: String::new(),
                new_game_dir_disk: String::new(),
                new_game_dir_relative: String::new(),
                new_exe_disk: String::new(),
                new_exe_relative: String::new(),
                auto_filled_dir: String::new(),
                auto_filled_name: String::new(),
                creating: false,
                create_msg: None,
                add_match: AddMatch::default(),
                cloud_pick: CloudPick::default(),
                wine_prefix_input: String::new(),
                wine_prefix_dirty: false,
                wine_status: None,
                wine_msg: None,
                service_busy: false,
                service_msg: None,
                daemon_paused: false,
                picker: None,
                picking: false,
                picked_path: None,
                pick_token: 0,
                environment: None,
                process_picker: ProcessPicker::default(),
                confirm_stop: false,
                config_source: ConfigSource::default(),
                config_switching: false,
                config_msg: None,
                retry_attempts: 0,
                running: std::collections::BTreeMap::new(),
                sync_status: None,
                sync_form: SyncForm::default(),
                sync_restore_pending: None,
                versions: VersionsState::default(),
            },
            Task::batch([
                Task::perform(async { connect_and_load().await }, Message::GamesLoaded),
                Task::perform(
                    async { load_wine_status().await },
                    Message::WineStatusLoaded,
                ),
                Task::perform(async { load_sync_status().await }, |result| {
                    Message::SyncStatusLoaded(Box::new(result))
                }),
                // 「浏览…」能不能用,开机就问一次(没有对话框的桌面上按钮要灰着并说明)。
                Task::perform(
                    async { crate::picker::probe().await },
                    Message::PickerProbed,
                ),
                // Start the periodic session poll.
                Task::perform(async { tokio::time::sleep(STATUS_POLL).await }, |_| {
                    Message::Tick
                }),
            ]),
        )
    }

    /// 单游戏设置页正在看的那条库里的游戏。
    ///
    /// 注意是**库里的**(已存值),不是 `draft`:页面的可编辑副本以它为准来抄,
    /// 拿草稿去填会把用户正在输入的内容一次次重置。
    pub(super) fn selected_game(&self) -> Option<&UiGame> {
        let id = self.selected.as_deref()?;
        self.games.iter().find(|game| game.id == id)
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

    /// 凭据现在存在哪一级(ADR-014 的三级存储)。还没读到 `sync.status` 时按系统
    /// 密钥环算,与页面上的默认一致 —— 第一条消息不先把用户吓一跳。
    pub(super) fn credential_store(&self) -> CredentialStore {
        self.sync_status
            .as_ref()
            .map(SyncStatus::store)
            .unwrap_or_default()
    }

    /// Re-read the sync status after a change.
    pub(super) fn reload_sync(&self) -> Task<Message> {
        Task::perform(async { load_sync_status().await }, |result| {
            Message::SyncStatusLoaded(Box::new(result))
        })
    }

    // ── 自动保存(没有「保存」按钮了,这一节就是那个按钮) ──────────────────

    /// 一笔编辑之后安排一次自动保存。
    ///
    /// 不直接写:每一笔编辑都把世代 +1、挂一个 [`AUTOSAVE_DEBOUNCE`] 的定时器,定时器
    /// 醒来时**世代对不上就不做**(说明这 700ms 里用户又改了)。所以连打一串字只写一次,
    /// 而且永远写的是最后一次编辑之后的那份草稿。
    pub(super) fn schedule_auto_save(&mut self) -> Task<Message> {
        if self.draft.is_none() {
            return Task::none();
        }
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
        let generation = self.autosave_generation;
        Task::perform(
            async move { tokio::time::sleep(AUTOSAVE_DEBOUNCE).await },
            move |_| Message::AutoSave(generation),
        )
    }

    /// 让还挂在防抖窗口里的那一次作废(换游戏、返回列表、按「重置」)。
    pub(super) fn cancel_auto_save(&mut self) {
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
    }

    /// 真的发一次保存。除了防抖到点,「重置」也直接调它(那是明确的一次操作,不用等)。
    ///
    /// **同一时刻只允许一笔**:daemon 并发处理请求,两次全量写若重叠,后到的旧快照会
    /// 把新的盖掉。所以这里见到在路上的就退回 —— 不用担心丢掉这一次,在路上的那笔回来
    /// 时世代必然已经变了(见 `Message::ProfileSaved`),它会自己再存一遍。
    ///
    /// 按按钮那一组走同一条通道(`SaveGroup`);按钮在 `saving` 时是禁用的,所以
    /// "点了没反应"这件事不会发生。
    pub(super) fn begin_auto_save(&mut self) -> Task<Message> {
        self.begin_save(SaveScope::Auto)
    }

    pub(super) fn begin_save(&mut self, scope: SaveScope) -> Task<Message> {
        let Some(draft) = self.draft.clone() else {
            return Task::none();
        };
        if self.save_in_flight.is_some() {
            tracing::debug!("上一笔保存还没回来，这一次等它");
            return Task::none();
        }
        self.saving = true;
        let generation = self.autosave_generation;
        self.save_in_flight = Some(SaveAttempt {
            draft: draft.clone(),
            scope,
        });
        Task::perform(
            async move { save_profile(draft, scope).await },
            move |result| Message::ProfileSaved(generation, result),
        )
    }

    /// 单游戏设置页底部那行小字:说一句话,并说明它是好消息还是坏消息。
    ///
    /// 光有一句话不够 —— 渲染那层要知道用绿色还是红色,而"猜前缀"是靠不住的。
    pub(super) fn report_saved(&mut self, message: impl Into<String>, ok: bool) {
        self.saved_msg = Some(message.into());
        self.saved_ok = ok;
    }

    // ── 「浏览…」:借系统自己的对话框挑一个位置 ──────────────────────────────

    /// 按钮能不能点:探到了对话框,而且现在没有另一个框开着。
    pub(super) fn can_browse(&self) -> bool {
        !self.picking && matches!(self.picker, Some(Ok(())))
    }

    /// 按钮下面那行灰字。能用时是空串(什么都不显示)。
    pub(super) fn path_hint(&self) -> String {
        match &self.picker {
            Some(Err(reason)) => {
                format!("「浏览…」在这台机器上用不了:{reason}。这里的位置可以照常自己填。")
            }
            _ => String::new(),
        }
    }

    /// 拼一个选择请求:挑什么(文件还是目录)、标题、以及从哪儿开始找。
    ///
    /// 起点只是"方便",不是规则 —— 认不出合适的起点就让对话框自己决定,别拿一个不存在
    /// 的目录去喂它(portal 会直接拒掉整个请求)。
    pub(super) fn pick_request(&self, target: PathTarget) -> crate::picker::Request {
        use crate::picker::Request;

        let draft = self.draft.as_ref();
        match target {
            PathTarget::NewGameDir => {
                Request::folder("选游戏根目录", existing_dir(&self.new_game_dir))
            }
            PathTarget::NewExe => Request::exe(
                "选可执行文件",
                parent_dir(&self.new_exe).or_else(|| existing_dir(&self.new_game_dir)),
            ),
            PathTarget::GameDir => Request::folder(
                "选游戏根目录",
                draft.and_then(|d| existing_dir(&d.game_dir)),
            ),
            PathTarget::Exe => Request::exe(
                "选可执行文件",
                draft.and_then(|d| parent_dir(&d.exe).or_else(|| existing_dir(&d.game_dir))),
            ),
            PathTarget::SavePath(index) => {
                let entry = draft.and_then(|d| d.save_paths.get(index));
                let game_dir = draft.and_then(|d| existing_dir(&d.game_dir));
                match entry.map(|entry| entry.kind.as_str()) {
                    // relative 只可能是游戏目录里的东西 —— 起点就放在那儿。
                    Some("relative") => Request::folder("选存档目录(要在游戏根目录里面)", game_dir),
                    Some("absolute") => Request::folder(
                        "选存档目录",
                        entry.and_then(|entry| existing_dir(&entry.path)),
                    ),
                    // windows:存档在 prefix 的用户目录里,能认出来就从那儿开始找
                    // (认路径形状,不需要先知道游戏用的是哪个 prefix,见 `wine.rs`)。
                    _ => Request::folder(
                        "选存档目录(prefix 里的 Windows 路径)",
                        self.prefix_user_dir(),
                    ),
                }
            }
            PathTarget::WinePrefix => Request::folder(
                "选 wine 目录(prefix)",
                existing_dir(&self.wine_prefix_input).or_else(|| self.machine_prefix()),
            ),
            // 聊天框问的是"程序在哪"，用户给的多半是**目录**（"kopia 进程所在目录"），
            // 所以对话框也按目录来 —— 想指具体某个文件的话，输入框里手打就行。
            PathTarget::RcloneBinary => Request::folder(
                "选 rclone 所在目录（也可以直接在输入框里写完整路径）",
                existing_dir(&self.sync_form.rclone_binary)
                    .or_else(|| parent_dir(&self.sync_form.rclone_binary)),
            ),
            PathTarget::KopiaBinary => Request::folder(
                "选 kopia 所在目录（也可以直接在输入框里写完整路径）",
                existing_dir(&self.sync_form.kopia_binary)
                    .or_else(|| parent_dir(&self.sync_form.kopia_binary)),
            ),
        }
    }

    /// 这台机器上现在生效的那个 wine prefix(`wine.status` 报的,可能还没有)。
    fn machine_prefix(&self) -> Option<PathBuf> {
        let status = self.wine_status.as_ref()?;
        [
            status.configured.clone(),
            Some(status.default_prefix.clone()),
            status.detected.first().cloned(),
        ]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .find(|path| path.is_dir())
    }

    /// 那个 prefix 里的 Windows 用户目录(`drive_c/users/<用户>`)。
    fn prefix_user_dir(&self) -> Option<PathBuf> {
        let prefix = self.machine_prefix()?;
        let user = crate::wine::windows_user_dir(&prefix);
        user.is_dir().then_some(user)
    }
}

/// 一个输入框里的目录(不存在、或者还空着就是 `None`)。
fn existing_dir(text: &str) -> Option<PathBuf> {
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

/// 一个输入框里的文件的上级目录。
fn parent_dir(text: &str) -> Option<PathBuf> {
    Path::new(text.trim())
        .parent()
        .map(Path::to_path_buf)
        .filter(|path| path.is_dir())
}

/// 从对话框挑回来的路径怎么落到各处（以及挑完顺手认一次盘）。
mod picker;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_detail;

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
    pub(super) creating: bool,
    pub(super) create_msg: Option<String>,
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
                saved_ok: true,
                autosave_generation: 0,
                save_in_flight: None,
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
                service_busy: false,
                service_msg: None,
                daemon_paused: false,
                picker: None,
                picking: false,
                picked_path: None,
                pick_token: 0,
                environment: None,
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
    pub(super) fn begin_auto_save(&mut self) -> Task<Message> {
        let Some(draft) = self.draft.clone() else {
            return Task::none();
        };
        if self.save_in_flight.is_some() {
            tracing::debug!("上一笔自动保存还没回来，这一次等它");
            return Task::none();
        }
        self.saving = true;
        let generation = self.autosave_generation;
        self.save_in_flight = Some(SaveAttempt {
            draft: draft.clone(),
        });
        Task::perform(async move { save_profile(draft).await }, move |result| {
            Message::ProfileSaved(generation, result)
        })
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

    /// 对话框里挑回来的路径,填进对应的那个框。
    ///
    /// 单游戏设置页的目标还要多做一件事:值写进 `draft`、按改动自动保存,同时记下这次
    /// 填的是什么 —— 那些输入框由**页面自己**持有,Rust 平时不往里写(见
    /// `game-settings.slint` 的文件头),所以 `render` 要靠这条记录推一次。
    pub(super) fn apply_picked_path(&mut self, target: PathTarget, picked: &Path) -> Task<Message> {
        let text = picked.display().to_string();

        match target {
            PathTarget::NewGameDir => self.new_game_dir = text,
            PathTarget::NewExe => self.new_exe = text,
            PathTarget::WinePrefix => {
                self.wine_prefix_input = text;
                // 用户亲手选的路径不许被随后回来的 `wine.status` 盖掉。
                self.wine_prefix_dirty = true;
            }
            // 两个"程序位置"是 `[sync]` 里的设置项，所以它们和 bucket 那些一样是**表单
            // 的一部分**（随「保存设置」一起提交），只是另有一个浏览按钮帮着填。
            PathTarget::RcloneBinary => {
                self.sync_form.rclone_binary = text;
                self.sync_form.settings_dirty = true;
            }
            PathTarget::KopiaBinary => {
                self.sync_form.kopia_binary = text;
                self.sync_form.settings_dirty = true;
            }
            PathTarget::GameDir | PathTarget::Exe => {
                let Some(draft) = self.draft.as_mut() else {
                    return Task::none();
                };
                if target == PathTarget::GameDir {
                    draft.game_dir = text.clone();
                } else {
                    draft.exe = text.clone();
                }
                self.picked_path = Some((target, text));
                return self.schedule_auto_save();
            }
            PathTarget::SavePath(index) => {
                // 挑回来的路径**自己**说明它属于哪一类(相对 → 令牌 → 绝对),
                // 所以这里顺手把类型也改对,再按那一类翻译。
                //
                // 从前是拿**当前选中的类型**去翻译,类型与路径不符就报错、什么都不改 ——
                // 而真 Windows 上挑回来的必然是 `C:\Users\…`,当时那个 windows 分支只认
                // wine 的 `drive_c/users/…` 形状,于是"点浏览没反应"(BUG-REPORT
                // 「存档位置的浏览有问题」)。顺序与取舍见 `wine::portable_save_path`。
                let (kind, value) = {
                    let Some(draft) = self.draft.as_ref() else {
                        return Task::none();
                    };
                    if draft.save_paths.get(index).is_none() {
                        return Task::none();
                    }
                    let (kind, text) =
                        crate::wine::portable_save_path(Path::new(draft.game_dir.trim()), picked);
                    (kind.as_str().to_string(), text)
                };
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|draft| draft.save_paths.get_mut(index))
                {
                    entry.kind = kind;
                    entry.path = value.clone();
                }
                self.picked_path = Some((target, value));
                return self.schedule_auto_save();
            }
        }
        Task::none()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

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
        let _ = app.update(Message::SyncStatusLoaded(Box::new(Ok(status))));

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
            Message::SyncField(SyncField::Bucket, "my-own-bucket".into()),
        ] {
            let _ = app.update(message);
        }

        // A successful save echoes nothing back and clears only what it took —
        // and it leaves the (still unsaved) bucket edit alone even though a
        // reload follows.
        let _ = app.update(Message::SyncCredentialsSaved(Ok(())));
        assert!(app.sync_form.key_id.is_empty());
        assert!(app.sync_form.app_key.is_empty());
        assert_eq!(app.sync_form.bucket, "my-own-bucket");

        // Once saved, the daemon is the truth again. (`Ok(false)` = 没换引擎，没有那条警告。)
        let _ = app.update(Message::SyncSettingsSaved(Ok(false)));
        assert!(!app.sync_form.settings_dirty);
        app.sync_form.apply(&status, &sync_payload()["settings"]);
        assert_eq!(app.sync_form.bucket, "kotori-saves");

        // A failed save keeps the user's text so they can correct it.
        let _ = app.update(Message::SyncField(SyncField::Bucket, "typo".into()));
        let _ = app.update(Message::SyncSettingsSaved(Err("boom".into())));
        app.sync_form.apply(&status, &sync_payload()["settings"]);
        assert_eq!(app.sync_form.bucket, "typo");
    }

    /// 点「kopia」必须**当场提交**，而不是只改表单等「保存设置」。
    ///
    /// 回归测试：从前 `SyncEngineSelected` 只改 `sync_form.engine`，而按钮上立刻显示成
    /// 「kopia ✓」—— 用户以为选了，config 里一个字节都没变，重开 GUI 又回到 rclone
    /// （2026-09-16 报的）。所以这里断言的是"这一次点击**产生了一次提交**"，而不是
    /// "表单变了" —— 表单变了正是当初唯一发生的事情，它证明不了任何事。
    #[test]
    fn picking_an_engine_submits_it_in_the_same_click() {
        let (mut app, _task) = App::new();
        let effects = app
            .update(Message::SyncEngineSelected("kopia".into()))
            .into_effects();
        assert_eq!(effects.len(), 1, "点引擎必须当场提交");
        assert_eq!(app.sync_form.engine, "kopia");
    }

    /// 换引擎那一笔**只管 engine 一个字段**：用户手上还没保存的编辑不能被当成已保存。
    ///
    /// 这是"点一下就生效"赖以成立的前提（daemon 的 `SettingsPatch` 是按字段合并的）。
    /// 少了这条保护，点一下引擎就会让随后回来的 `sync.status` 把用户正在填的 bucket
    /// 覆盖掉 —— 那比"点了没反应"更难查。
    #[test]
    fn switching_the_engine_leaves_unsaved_edits_alone() {
        let (mut app, _task) = App::new();
        app.sync_form.bucket = "typed-but-not-saved".into();
        app.sync_form.settings_dirty = true;

        let _ = app.update(Message::SyncEngineSelected("kopia".into()));
        assert!(
            app.sync_form.settings_dirty,
            "换引擎不该把别的编辑标成已保存"
        );
        assert_eq!(app.sync_form.bucket, "typed-but-not-saved");

        // 提交失败时反过来：别让界面继续装着已经换了 —— 清掉 dirty，好让下面这次
        // 刷新把配置里真正的值拉回来。
        let _ = app.update(Message::SyncEngineSaved(Err("daemon 不在了".into())));
        assert!(!app.sync_form.settings_dirty);
    }

    /// 用户自己按的「停止服务」必须真的停得住 —— 界面的自愈逻辑(轮询后的重连、
    /// 失败退避重试)不能在三秒内把它又拉起来。
    #[test]
    fn stopping_the_service_by_hand_is_not_undone_by_the_ui() {
        let (mut app, _task) = App::new();
        app.daemon_connected = Some(true);

        let _ = app.update(Message::ServiceStop);
        assert!(app.daemon_paused && app.service_busy);
        let _ = app.update(Message::ServiceStopped(Ok("后台服务已停止".into())));
        assert_eq!(app.daemon_connected, Some(false));
        assert!(!app.service_busy);
        // 说清后果:正在玩的那一局不受影响。
        assert!(
            app.service_msg
                .as_deref()
                .unwrap_or_default()
                .contains("不受影响"),
            "{:?}",
            app.service_msg
        );

        // 刷新与失败重试都不许把它拉回来。
        let _ = app.update(Message::Refresh);
        let _ = app.update(Message::GamesLoaded(Err("无法连接守护进程".into())));
        assert!(app.daemon_paused);
        assert_eq!(app.retry_attempts, 0, "停掉的服务不该进退避重试循环");

        // 「启动服务」之后牌子摘掉,连接状态由回包摆正。
        let _ = app.update(Message::ServiceStart);
        assert!(app.service_busy);
        let _ = app.update(Message::ServiceStarted(Ok("后台服务已启动".into())));
        assert!(!app.daemon_paused);
        let _ = app.update(Message::GamesLoaded(Ok(vec![ui_game()])));
        assert_eq!(app.daemon_connected, Some(true));
        assert_eq!(app.retry_attempts, 0);
    }

    /// 没停成 / 别处又起了一个:界面必须说实话,不能一边"已连接"一边"已停止"。
    #[test]
    fn a_service_that_is_alive_again_clears_the_stopped_flag() {
        let (mut app, _task) = App::new();
        let _ = app.update(Message::ServiceStop);
        let _ = app.update(Message::ServiceStopped(Err("拒绝连接".into())));
        assert!(!app.daemon_paused, "没停掉就别立那块牌子");
        assert!(
            app.service_msg
                .as_deref()
                .unwrap_or_default()
                .contains("失败")
        );

        // 从命令行起的守护进程:会话轮询有回应 ⇒ 摘牌。
        let _ = app.update(Message::ServiceStop);
        assert!(app.daemon_paused);
        let _ = app.update(Message::StatusLoaded(Ok(Default::default())));
        assert!(!app.daemon_paused);
        assert_eq!(app.daemon_connected, Some(true));
    }

    /// 自动保存:连改几笔只会存最后一笔,而且过期回包必须让**新的**那份再存一次 ——
    /// 否则配置里留下的是用户已经改掉的值。
    #[test]
    fn the_latest_edit_is_the_one_that_gets_saved() {
        let (mut app, _task) = App::new();
        app.games = vec![ui_game()];
        app.update(Message::GameSelected("demo".into()));

        // 第一笔:挂一个防抖定时器,而不是立刻写。
        app.update(Message::AlgoChanged("Nis".into()));
        assert_eq!(app.autosave_generation, 1);
        assert!(
            app.save_in_flight.is_none() && !app.saving,
            "还没到点,不该发出去"
        );

        // 用户还在改:旧的定时器醒来时世代已经对不上,作废。
        app.update(Message::SharpnessChanged(4.0));
        app.update(Message::AutoSave(1));
        assert!(app.save_in_flight.is_none(), "过期的定时器不许写");

        // 最后那一笔到点,才真的发出去。
        app.update(Message::AutoSave(2));
        assert!(app.saving);
        let in_flight = app
            .save_in_flight
            .as_ref()
            .expect("一笔应该在路上")
            .draft
            .clone();
        assert_eq!(in_flight.algo, "Nis");
        assert_eq!(in_flight.sharpness, 4);

        // 上一笔还没回来时又改了一笔:不发第二笔(并发写会互相覆盖)……
        app.update(Message::FramerateChanged("60".into()));
        app.update(Message::AutoSave(3));
        assert_eq!(
            app.save_in_flight
                .as_ref()
                .map(|a| a.draft.framerate.clone()),
            Some(String::new()),
            "在路上的那笔不该被替换"
        );

        // ……等它回来时发现世代变了,于是拿手上的草稿再存一次。
        app.update(Message::ProfileSaved(2, Ok(())));
        assert!(app.saving);
        assert_eq!(
            app.save_in_flight
                .as_ref()
                .map(|a| a.draft.framerate.clone()),
            Some("60".to_string()),
            "过期回包之后要把最新的那份补上"
        );

        let task = app.update(Message::ProfileSaved(3, Ok(())));
        assert!(!app.saving && app.save_in_flight.is_none());
        assert_eq!(app.saved_msg.as_deref(), Some("已自动保存"));
        // 存成功后要重读一遍库,列表里的"已存值"才跟得上。
        assert_eq!(task.into_effects().len(), 1);
    }

    /// 一条回包只能落在它自己那一份草稿上:用户中途翻到别的游戏时,不能把别人的
    /// `*_original` 写成这个游戏的值。
    #[test]
    fn a_late_save_never_touches_another_games_draft() {
        let (mut app, _task) = App::new();
        app.games = vec![
            ui_game(),
            UiGame {
                id: "other".into(),
                name: "Other".into(),
                ..ui_game()
            },
        ];
        app.update(Message::GameSelected("demo".into()));
        app.update(Message::ExePathChanged("/games/demo/renamed.exe".into()));
        app.update(Message::AutoSave(app.autosave_generation));
        assert!(app.save_in_flight.is_some());

        app.update(Message::GameSelected("other".into()));
        app.update(Message::ProfileSaved(1, Ok(())));
        let draft = app.draft.as_ref().expect("换过去的游戏也有草稿");
        assert_eq!(draft.game_id, "other");
        assert_eq!(draft.exe_original, draft.exe, "别人的书签不许被动");
        assert!(app.saved_msg.is_none(), "已经离开那一页了,别在这儿报");
    }

    /// 「浏览…」挑回来的路径要落到**它自己那个**目标上,并且该自动保存的立刻挂上。
    ///
    /// 串了目标的话,用户会在另一个框里看到刚挑的路径 —— 而那种错误在编译期完全看不出来。
    #[test]
    fn a_picked_path_lands_on_its_own_field_and_schedules_a_save() {
        let (mut app, _task) = App::new();
        app.games = vec![ui_game()];
        app.update(Message::GameSelected("demo".into()));

        // 单游戏页:游戏根目录 → 草稿 + 推给页面的令牌 + 一次自动保存。
        let generation = app.autosave_generation;
        app.apply_picked_path(PathTarget::GameDir, Path::new("/games/other"));
        let draft = app.draft.as_ref().unwrap();
        assert_eq!(draft.game_dir, "/games/other");
        assert_eq!(draft.exe, ui_game().exe, "别的一个字都不该动");
        assert_eq!(
            app.picked_path.as_ref().map(|(t, v)| (*t, v.as_str())),
            Some((PathTarget::GameDir, "/games/other"))
        );
        assert_eq!(app.autosave_generation, generation + 1);

        // 存档行的相对路径:挑游戏目录里面的位置 → 存成相对的。
        app.draft.as_mut().unwrap().save_paths = vec![SavePathDraft {
            kind: "relative".into(),
            path: "savedata".into(),
            exclude: String::new(),
        }];
        app.draft.as_mut().unwrap().game_dir = "/games/other".into();
        app.apply_picked_path(
            PathTarget::SavePath(0),
            Path::new("/games/other/savedata/backup"),
        );
        assert_eq!(
            app.draft.as_ref().unwrap().save_paths[0].path,
            "savedata/backup"
        );

        // 挑到游戏目录外面:退到下一档。先试令牌(不在用户目录里,不成),再退成
        // **绝对路径** —— 用户点了浏览总得有个结果,而"这条只在这台机器上成立"由 kind
        // 那一栏自己说清楚(它会从 relative 变成 absolute)。
        //
        // ⚠ 这条断言 2026-09-18 变了:从前是"翻译不了就什么都不改 + 报一句红字",
        // 那个规矩在真机上表现成"点了浏览没反应"(见 `wine::portable_save_path`)。
        app.apply_picked_path(PathTarget::SavePath(0), Path::new("/elsewhere/save"));
        let entry = &app.draft.as_ref().unwrap().save_paths[0];
        assert_eq!(entry.kind, "absolute");
        assert_eq!(entry.path, "/elsewhere/save");

        // 真 Windows 上从资源管理器挑回来的形状 —— 这条是那个 bug 的正面:
        // 挑回来必然长这样,而它**必须**自己认出来是令牌形态,并把类型一起改对。
        app.apply_picked_path(
            PathTarget::SavePath(0),
            Path::new(r"C:\Users\tester\AppData\Roaming\Game\save"),
        );
        let entry = &app.draft.as_ref().unwrap().save_paths[0];
        assert_eq!(entry.kind, "windows");
        assert_eq!(entry.path, r"%APPDATA%\Game\save");

        // 添加游戏页的两个框走各自的字段,不进草稿,也不用那个推给页面的令牌。
        let stale = app.picked_path.clone();
        app.apply_picked_path(PathTarget::NewGameDir, Path::new("/games/new"));
        app.apply_picked_path(PathTarget::NewExe, Path::new("/games/new/game.exe"));
        assert_eq!(app.new_game_dir, "/games/new");
        assert_eq!(app.new_exe, "/games/new/game.exe");
        assert_eq!(app.picked_path, stale, "这两个框由页面从状态读,不用令牌");

        // wine prefix:填上,并立起"别被随后回来的 wine.status 盖掉"的旗。
        app.apply_picked_path(PathTarget::WinePrefix, Path::new("/prefixes/games"));
        assert_eq!(app.wine_prefix_input, "/prefixes/games");
        assert!(app.wine_prefix_dirty);
    }

    /// 没有对话框的机器上按钮不能点,而且要说清为什么(用户 2026-09-13:"没有就不能用")。
    #[test]
    fn browsing_is_refused_when_the_machine_has_no_dialog() {
        let (mut app, _task) = App::new();
        assert!(!app.can_browse(), "还没探完就先别放行");

        app.update(Message::PickerProbed(Err("没有 xdg-desktop-portal".into())));
        assert!(!app.can_browse());
        assert!(
            app.path_hint().contains("xdg-desktop-portal"),
            "{}",
            app.path_hint()
        );

        app.update(Message::PickerProbed(Ok(())));
        assert!(app.can_browse());
        assert!(app.path_hint().is_empty(), "能用的时候一个字都不显示");

        // 框开着的时候不给再开一个(点两下 = 弹两个对话框)。
        app.update(Message::PickPath(PathTarget::GameDir));
        assert!(app.picking && !app.can_browse());

        // 用户点了取消:不是失败,更不许把按钮灰掉。⚠ 真机上踩过 —— 点一次叉号,
        // 「浏览…」就永久灰了,因为这条回包当时被写成了"这台机器没有对话框"。
        app.update(Message::PathPicked(PathTarget::GameDir, Ok(None)));
        assert!(app.can_browse(), "取消之后还得能再点");
        assert!(app.error.is_none(), "取消不是错误:{:?}", app.error);

        // 真出错了也不灰:那是"这一次没成",不是"这台机器没有对话框"。
        app.update(Message::PathPicked(
            PathTarget::GameDir,
            Err("DBus 断了".into()),
        ));
        assert!(app.can_browse(), "失败之后按钮也得留着");
        assert!(
            app.error
                .as_deref()
                .unwrap_or_default()
                .contains("DBus 断了"),
            "{:?}",
            app.error
        );
        assert!(
            app.path_hint().is_empty(),
            "这不代表这台机器没有对话框:{:?}",
            app.path_hint()
        );
    }

    /// 每次"请对话框出来"都要带上合适的标题与类别:目录 / 文件这两类认错了,
    /// 用户会在一个"选 exe"的框里被逼着选目录。
    #[test]
    fn a_pick_request_says_what_kind_of_thing_it_wants() {
        use crate::picker::Want;

        let (mut app, _task) = App::new();
        app.games = vec![ui_game()];
        app.update(Message::GameSelected("demo".into()));

        assert_eq!(app.pick_request(PathTarget::GameDir).want, Want::Folder);
        assert_eq!(app.pick_request(PathTarget::Exe).want, Want::Exe);
        assert_eq!(app.pick_request(PathTarget::NewExe).want, Want::Exe);
        assert_eq!(app.pick_request(PathTarget::WinePrefix).want, Want::Folder);
    }

    /// 「重置」:回到已存值、作废还挂着的防抖,而且"没东西可还原"时要如实说。
    #[test]
    fn reset_goes_back_to_the_stored_settings() {
        let (mut app, _task) = App::new();
        app.games = vec![ui_game()];
        app.update(Message::GameSelected("demo".into()));

        app.update(Message::ResetProfile);
        assert_eq!(
            app.saved_msg.as_deref(),
            Some("没有未保存的改动"),
            "本来就没改,别装作还原了什么"
        );

        app.update(Message::GameDirChanged("/games/elsewhere".into()));
        let generation = app.autosave_generation;
        app.update(Message::ResetProfile);
        assert_eq!(app.autosave_generation, generation + 1, "挂着的那笔要作废");
        assert_eq!(
            app.draft.as_ref().map(|d| d.game_dir.clone()),
            Some("/games/demo".into())
        );
        assert_eq!(app.saved_msg.as_deref(), Some("已还原为已保存的设置"));
        assert!(!app.draft.as_ref().unwrap().game_dir_changed());
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
            Some("已从系统密钥环里删除 B2 凭据")
        );

        // A failure keeps what the user typed and names the problem.
        let _ = app.update(Message::SyncField(SyncField::KeyId, "0046b5".into()));
        let _ = app.update(Message::SyncCredentialsCleared(
            Err("密钥环没在运行".into()),
        ));
        assert_eq!(app.sync_form.key_id, "0046b5");
        assert!(
            app.sync_form
                .msg
                .as_deref()
                .unwrap_or_default()
                .contains("密钥环没在运行")
        );
    }

    /// 凭据到底存在哪一级,消息里就得说哪一级 —— 没有密钥环的机器上凭据只在内存里,
    /// 说成"已存入系统密钥环"就是在骗用户(他会以为重启之后还在)。
    #[test]
    fn the_save_message_names_the_store_that_actually_holds_the_credentials() {
        let (mut app, _task) = App::new();

        // 本机没有可用的密钥环:`sync.status` 报 session-only。
        app.sync_status = Some(SyncStatus {
            store_kind: "session-only".into(),
            ephemeral: true,
            ..sync_status_fixture()
        });
        assert_eq!(app.credential_store(), CredentialStore::Session);
        let _ = app.update(Message::SyncCredentialsSaved(Ok(())));
        let memory = app.sync_form.msg.clone().unwrap_or_default();
        assert!(!memory.contains("系统密钥环"), "{memory}");
        assert!(memory.contains("本次会话的内存"), "{memory}");
        assert!(memory.contains("主密码"), "要告诉用户怎么留住它:{memory}");

        // 主密码文件那一级:说"加密写入",不能说"磁盘上没有明文"。
        app.sync_status = Some(SyncStatus {
            store_kind: "encrypted-file".into(),
            ..sync_status_fixture()
        });
        assert_eq!(app.credential_store(), CredentialStore::File);
        let _ = app.update(Message::SyncCredentialsSaved(Ok(())));
        let sealed = app.sync_form.msg.clone().unwrap_or_default();
        assert!(sealed.contains("主密码凭据文件"), "{sealed}");
        assert!(!sealed.contains("磁盘上没有明文"), "{sealed}");

        // 还没读到状态时也别说谎:默认那级是系统密钥环,但名字来自同一处。
        let (mut fresh, _task) = App::new();
        assert!(fresh.sync_status.is_none());
        assert_eq!(fresh.credential_store(), CredentialStore::System);
        let _ = fresh.update(Message::SyncCredentialsSaved(Ok(())));
        assert!(
            fresh
                .sync_form
                .msg
                .as_deref()
                .unwrap_or_default()
                .contains("系统密钥环")
        );
    }

    /// 没有可持久化后端时,保存要被拒绝 —— **不许静默存进内存**。
    ///
    /// 内存那一级只是过渡态(命令行"先存凭据、再封进文件");没有密钥环的机器
    /// (含还没接凭据管理器的 Windows)必须先设主密码,否则用户以为存好了,重启就没了。
    #[test]
    fn nothing_is_saved_while_only_memory_is_available() {
        let (mut app, _task) = App::new();
        app.sync_status = Some(SyncStatus {
            store_kind: "session-only".into(),
            ephemeral: true,
            ..sync_status_fixture()
        });
        app.sync_form.apply(
            app.sync_status.as_ref().unwrap(),
            &sync_payload()["settings"],
        );

        // 凭据:拒绝,并指路到主密码那一行;框里的东西要留着(用户不用重打)。
        let _ = app.update(Message::SyncField(SyncField::KeyId, "0046b5".into()));
        let _ = app.update(Message::SyncField(SyncField::AppKey, "K004".into()));
        let _ = app.update(Message::SyncSaveCredentials);
        assert!(!app.sync_form.busy, "不该发请求");
        assert_eq!(app.sync_form.key_id, "0046b5");
        let refused = app.sync_form.msg.clone().unwrap_or_default();
        // 说法要落到"为什么存不住"上(现在是凭据文件写不下去,不再是"没有密钥环")。
        assert!(
            refused.contains("写权限") || refused.contains("内存"),
            "{refused}"
        );

        // 删除不受这条限制:那是"删掉",不是"存下来"。
        app.sync_form.busy = false;
        let _ = app.update(Message::SyncClearCredentials);
        assert!(app.sync_form.busy, "删凭据应该照样发出去");

        // 有了文件后端(或密钥环)就放行。
        app.sync_form.busy = false;
        app.sync_status = Some(SyncStatus {
            store_kind: "encrypted-file".into(),
            ..sync_status_fixture()
        });
        let _ = app.update(Message::SyncSaveCredentials);
        assert!(app.sync_form.busy, "有可持久化后端就该发出去");
    }

    /// 删除凭据文件是破坏性操作:没点过"删除"就直接确认,什么都不该发生。
    #[test]
    fn deleting_the_master_file_needs_the_confirmation_it_asked_for() {
        let (mut app, _task) = App::new();
        let _ = app.update(Message::SyncMasterDeleteConfirmed);
        assert!(!app.sync_form.busy, "没确认过就不该发请求");

        let _ = app.update(Message::SyncMasterDeleteRequested);
        assert!(app.sync_form.confirm_master_delete);
        let _ = app.update(Message::SyncMasterDeleteCancelled);
        assert!(!app.sync_form.confirm_master_delete);

        let _ = app.update(Message::SyncMasterDeleteRequested);
        let _ = app.update(Message::SyncMasterDeleteConfirmed);
        assert!(!app.sync_form.confirm_master_delete, "确认后要收起确认条");
        assert!(app.sync_form.busy);

        let _ = app.update(Message::SyncMasterDeleted(Ok(())));
        assert!(!app.sync_form.busy);
        assert!(
            app.sync_form
                .msg
                .as_deref()
                .unwrap_or_default()
                .contains("凭据一起消失"),
            "要说清后果:{:?}",
            app.sync_form.msg
        );
    }

    /// 「测试连接」点下去必须**立刻**有一句话可说。
    ///
    /// 它背后是一次真的网络往返(连桶 / 必要时建仓库 / 列一次快照),而 `busy` 只把按钮
    /// 变灰 —— 从前这里把 msg 清成了 `None`,于是最长几分钟里界面毫无动静,用户看到的
    /// 就是"点了没反应"(2026-09-18 报的)。「立即同步全部」一直都有这句,是这一个漏了。
    #[test]
    fn testing_the_connection_says_something_right_away() {
        let (mut app, _boot) = App::new();
        let _ = app.update(Message::SyncTest);
        assert!(app.sync_form.busy, "按下去就该进忙状态");
        let msg = app.sync_form.msg.clone().unwrap_or_default();
        assert!(msg.contains("测试连接"), "{msg}");
    }
}

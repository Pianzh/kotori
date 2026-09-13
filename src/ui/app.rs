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
    /// 全局快捷键的注册情况(设置页);`None` = 还没问到。
    pub(super) hotkeys: Option<HotkeyStatus>,
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
                hotkeys: None,
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
        // 表单里要有东西,才走"存了密码"那一支(空 = 清除)。
        app.sync_form.password = "hunter2hunter2".into();
        let _ = app.update(Message::SyncPasswordSaved(Ok(())));
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

        // 同步密码同理(加密密码丢了,连自己上传的存档都解不开)。
        let _ = app.update(Message::SyncField(
            SyncField::Password,
            "hunter2hunter2".into(),
        ));
        let _ = app.update(Message::SyncField(
            SyncField::PasswordAgain,
            "hunter2hunter2".into(),
        ));
        let _ = app.update(Message::SyncSavePassword);
        assert!(!app.sync_form.busy, "不该发请求");
        let refused = app.sync_form.msg.clone().unwrap_or_default();
        assert!(
            refused.contains("写权限") || refused.contains("内存"),
            "{refused}"
        );

        // 清空密码不受影响:那是"删掉",不是"存下来"。
        let _ = app.update(Message::SyncClearPassword);
        assert!(app.sync_form.busy, "清密码应该照样发出去");

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
}

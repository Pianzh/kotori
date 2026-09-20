//! 「浏览/凭据/连接」那一半的 app 测试:从 `tests.rs` 拆来(纯移动)。

use super::*;
use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

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

/// 用户 2026-09-20 的实测反馈:点「浏览…」挑回 exe 之后,下面两个框必须自己填好。
///
/// 从前只有**手打**那条路触发联动(`Message::NewExeChanged`),浏览是
/// `apply_picked_path` 里的一句直接赋值 —— 于是"手动敲路径会自动填,点浏览反而不填",
/// 而点浏览恰恰是最常用的那条路。
#[test]
fn browsing_for_an_exe_fills_the_dir_and_the_name_too() {
    let (mut app, _task) = App::new();

    app.apply_picked_path(PathTarget::NewExe, Path::new("/games/new/game.exe"));
    assert_eq!(app.new_exe, "/games/new/game.exe");
    assert_eq!(app.new_game_dir, "/games/new");
    assert_eq!(app.new_name, "game");

    // 再挑一次(换目录):上一次自动填的值跟着更新,不卡在旧路径上。
    app.apply_picked_path(PathTarget::NewExe, Path::new("/games/other/3days_chs.exe"));
    assert_eq!(app.new_game_dir, "/games/other");
    assert_eq!(app.new_name, "3days_chs");

    // 用户自己改过的名字不再被覆盖;根目录没被动过,照旧跟着 exe 走。
    app.new_name = "我改过的名字".into();
    app.apply_picked_path(PathTarget::NewExe, Path::new("/games/third/Game.exe"));
    assert_eq!(app.new_name, "我改过的名字");
    assert_eq!(app.new_game_dir, "/games/third");
}

/// 两个框留空也能添加:名字取 exe 文件名、根目录取 exe 所在目录(用户
/// 2026-09-20「或者说把这两个输入框改成可选,不填时自动填充」)。
///
/// 推不出名字时不许装作成功 —— 给一句话,别发 RPC(否则会在库里留下一条无名条目)。
#[test]
fn an_empty_name_and_dir_are_filled_in_from_the_exe_on_submit() {
    let (mut app, _task) = App::new();
    app.new_exe = "/games/new/3days.exe".into();
    let _ = app.update(Message::CreateRequested);
    assert!(app.creating, "两个框留空应当照常提交");
    assert_eq!(app.create_msg, None);

    let (mut app, _task) = App::new();
    app.new_exe = "/".into();
    let _ = app.update(Message::CreateRequested);
    assert!(!app.creating, "推不出名字就别提交");
    let message = app.create_msg.clone().unwrap_or_default();
    assert!(message.contains("游戏名"), "要说清缺的是名字:{message}");
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

//! `app.rs` 的测试:自动保存的三条规矩、草稿与回包的攻防。
//! 从 `mod.rs` 整体搬来(纯移动)—— 那边一半是测试,早就在 500 行硬线之外。

use super::*;

use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};
use crate::ui::update::run::{RunAction, run_action};

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
/// 「kopia √」—— 用户以为选了，config 里一个字节都没变，重开 GUI 又回到 rclone
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

/// `BUG-18` 的回归：A 的保存还在路上时切到 B、改 B —— B 那一笔不能没人发。
///
/// 从前 `ProfileSaved` 只在"还是同一款"时才补发，于是切款之后 B 的编辑会因为
/// "上一笔还在路上"被退回，而且再也没人发它 —— 用户看到的是"改了、没保存"
/// （GAP-3 一直说这条没有回归测试）。
#[test]
fn switching_games_mid_save_still_persists_the_new_edits() {
    let (mut app, _task) = App::new();
    app.games = vec![
        ui_game(),
        UiGame {
            id: "other".into(),
            name: "Other".into(),
            ..ui_game()
        },
    ];

    // A 改一笔，让保存上路。
    app.update(Message::GameSelected("demo".into()));
    app.update(Message::AlgoChanged("Nis".into()));
    app.update(Message::AutoSave(app.autosave_generation));
    assert!(app.save_in_flight.is_some(), "A 那一笔该在路上");

    // 切到 B 再改 B：这一笔因为"上一笔还在路上"发不出去（`begin_auto_save` 退回）。
    app.update(Message::GameSelected("other".into()));
    app.update(Message::SharpnessChanged(4.0));
    app.update(Message::AutoSave(app.autosave_generation));
    assert!(
        app.save_in_flight
            .as_ref()
            .is_some_and(|attempt| attempt.draft.game_id == "demo"),
        "在路上的仍然是 A 那一笔"
    );

    // A 的回包到了：世代已经变了（切款 + 改 B）⇒ 必须**补发**手上这份（B 的）。
    app.update(Message::ProfileSaved(1, Ok(())));
    let resend = app
        .save_in_flight
        .as_ref()
        .expect("B 那一笔必须被补发，否则它就丢了");
    assert_eq!(resend.draft.game_id, "other");
    assert_eq!(resend.draft.sharpness, 4.0);
}

/// 「启动 / 停止」那一颗按钮:**在跑的要去停,没在跑的才去启动**。
///
/// 用户 2026-09-20 实测报的 bug:详情页头部那颗按钮在游戏跑起来之后显示成「停止」,
/// 点下去还在启动 —— 文案与动作各判了一次,而其中一处永远发 `game.launch`。现在两者
/// 都看会话表(见 `update::run_action`),这条测试盯住的就是"别再分岔"。
#[test]
fn the_run_button_stops_a_running_game_instead_of_launching_it_again() {
    let (mut app, _task) = App::new();

    // 没在跑 → 启动:`launching` 立刻立起来(按钮随之变灰)。
    let _ = app.update(Message::ToggleRun("demo".into()));
    assert_eq!(app.launching.as_deref(), Some("demo"));
    app.launching = None;

    // 已经在跑 → 停。⚠ 旧代码在这里会去启动(把 `launching` 又立起来)。
    app.running.insert(
        "demo".into(),
        SessionInfo {
            session_id: "s1".into(),
            watch_only: false,
        },
    );
    let _ = app.update(Message::ToggleRun("demo".into()));
    assert_eq!(app.launching, None, "在跑的那一款该去停,而不是再启动一次");

    // 观测会话(别人启动的那一局)同样算"在跑":那颗按钮 = 停掉跟踪。
    assert_eq!(run_action(&app.running, "demo"), RunAction::Stop);
    app.running.clear();
    assert_eq!(run_action(&app.running, "demo"), RunAction::Launch);
}

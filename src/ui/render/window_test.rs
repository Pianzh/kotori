//! 不开窗口的整页测试:用 Slint 的测试后端把真窗口建出来。
//!
//! 编译器管不到运行时,而 UI 的错误恰恰都在运行时:`Select.options[selected]` 越界、
//! `for` 到空数组、某个下标→枚举的映射写反、属性忘了填 —— 都是"翻到那一页才炸"。
//! 它顶替的是旧 UI 那份 `views_construct_for_every_tab_and_state`,并且额外把窗口回调
//! 挨个 `invoke_*` 一遍,断言消息真的落到了 `App` 上。

use std::rc::Rc;

use super::*;
use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

/// 切到某一页。`tab` 是**窗口自己的属性**(点导航栏时由 .slint 直接改),所以只改
/// `app.tab` 的话页面根本不会实例化 —— 那些"渲染过了"的断言会全部空跑(踩过)。
fn show_tab(ui: &mut Ui, tab: Tab) {
    ui.app.tab = tab;
    ui.window.set_tab(match tab {
        Tab::Games => 0,
        Tab::Add => 1,
        Tab::Sync => 2,
        Tab::Settings => 3,
    });
}

#[test]
fn every_page_renders_without_a_display() {
    i_slint_backend_testing::init_no_event_loop();

    let window = AppWindow::new().expect("测试后端应该能建窗口");
    // `window` 马上要搬进 Ui(句柄不是 Clone 的),回调那一段再从弱引用借回来。
    let handle = window.as_weak();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("建一个 tokio runtime 只为拿 Handle");
    let games = Rc::new(VecModel::<GameItem>::default());
    let saves = Rc::new(VecModel::<SaveItem>::default());
    window.set_games(games.clone().into());
    window.set_saves(saves.clone().into());

    let mut ui = Ui {
        app: App::new().0,
        window,
        runtime: runtime.handle().clone(),
        games,
        saves,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    };

    // 游戏库:空列表 → 有列表 → 搜索命中 / 不命中 → 运行中 / 仅监视 → 启动中。
    render(&mut ui);
    ui.app.games = vec![ui_game()];
    ui.app.search = "demo".into();
    render(&mut ui);
    ui.app.search = "zzz".into();
    render(&mut ui);
    ui.app.search.clear();
    ui.app.launching = Some("demo".into());
    render(&mut ui);
    for watch_only in [false, true] {
        ui.app.running.insert(
            "demo".into(),
            SessionInfo {
                session_id: "s".into(),
                watch_only,
            },
        );
        render(&mut ui);
    }
    ui.app.running.clear();
    ui.app.launching = None;

    // 单游戏设置页:先让 render 把下拉的选项推上去,再打开页面 —— 顺序反了就是
    // 拿空数组当下拉,真机上会看到一行报错。
    ui.app.selected = Some("demo".into());
    ui.app.draft = Some(Draft::from_game(&ui_game()));
    render(&mut ui);
    ui.window.set_game_open(true);
    render(&mut ui);

    // 三种形态的存档位置各一条(下拉、占位符、删除按钮都要能用)。
    let kinds = ["windows", "relative", "absolute"];
    ui.app.draft = Some(Draft {
        save_paths: kinds
            .iter()
            .map(|kind| SavePathDraft {
                kind: (*kind).to_string(),
                path: format!("/{kind}"),
                exclude: "*.log".into(),
            })
            .collect(),
        ..Draft::from_game(&ui_game())
    });
    render(&mut ui);

    ui.app.confirm_delete = true;
    render(&mut ui);
    ui.app.saved_msg = Some("已保存并通知守护进程".into());
    render(&mut ui);

    // 仅观测的游戏(页面上会多一节,内容来自 process_name)。
    ui.app.games = vec![UiGame {
        watch_only: true,
        process_name: "game.exe".into(),
        ..ui_game()
    }];
    render(&mut ui);
    ui.app.games = vec![ui_game()];

    // 添加游戏:空表单 → 填好 → 有回话。
    show_tab(&mut ui, Tab::Add);
    ui.app.selected = None;
    ui.app.draft = None;
    ui.app.confirm_delete = false;
    render(&mut ui);
    ui.app.new_name = "Demo".into();
    ui.app.new_game_dir = "/games/demo".into();
    ui.app.new_exe = "/games/demo/game.exe".into();
    render(&mut ui);
    ui.app.create_msg = Some("已添加（ID: demo）".into());
    render(&mut ui);

    // 云同步:还没读到 → 一切就绪 → 待确认加密 → 待确认恢复 → 凭据文件锁着 →
    // 本机连密钥环都没有 → 没装 rclone 且有个存档位置解析不了。
    show_tab(&mut ui, Tab::Sync);
    render(&mut ui);
    ui.app.sync_status = Some(sync_status_fixture());
    ui.app.sync_form.apply(
        ui.app.sync_status.as_ref().unwrap(),
        &sync_payload()["settings"],
    );
    render(&mut ui);

    ui.app.sync_form.confirm_encryption = Some(true);
    render(&mut ui);
    ui.app.sync_form.confirm_encryption = None;
    ui.app.sync_restore_pending = Some(("demo".into(), None));
    render(&mut ui);
    ui.app.sync_restore_pending = None;

    // 每一段都不能比窗口宽:输入框的 min-width 一旦等于 preferred-width,整页会被撑出去
    // (卡片被切、说明文字挤成一列竖字,见 UI_GUIDE §7.14)。凭据组是这一页最宽的一行
    // (说明 + 输入框 + 最多三个按钮),所以每个状态都量一次它。
    // ⚠ 这条只能在整页测试里做:测试后端才拿得到元素几何。
    let fits = |ui: &Ui, type_names: &[&str]| {
        let window = &ui.window;
        let width = window.window().size().width as f32 / window.window().scale_factor();
        for type_name in type_names {
            let found: Vec<_> = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
                window, type_name,
            )
            .collect();
            // 查不到就是查不到:没有调试信息的生成代码会静默返回空列表,这条断言就白写了
            // (见 build.rs 的 `with_debug_info`)。
            assert!(
                !found.is_empty(),
                "查不到 {type_name} —— Slint 的 ElementHandle 需要带调试信息的生成代码"
            );
            for element in found {
                assert!(
                    element.size().width <= width + 1.0,
                    "{type_name} 宽 {} 超过了窗口宽 {width}",
                    element.size().width
                );
            }
        }
    };
    let sync_states = ["SyncCredentialsGroup", "CardRow"];

    // ⚠ `ElementHandle` 只看得见**没被裁掉**的部分(`ItemRc::is_visible` 判的是裁剪矩形):
    // 740 高的窗口里凭据组刚好在折线以下,所以量之前先把窗口撑高 —— 否则查询会静默返回空,
    // 断言等于没写(这一点踩过一次)。
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2600.0));
    render(&mut ui);

    ui.app.sync_status = Some(SyncStatus {
        store_kind: "encrypted-file".into(),
        store_locked: true,
        master_file: "/home/user/.config/kotori/secrets.json".into(),
        keyring: "主密码加密文件（已锁定）".into(),
        ready: false,
        problem: Some("凭据文件已锁定，请先用主密码解锁".into()),
        ..sync_status_fixture()
    });
    ui.app.sync_form.master_password = "typed".into();
    render(&mut ui);
    fits(&ui, &sync_states);

    // 解锁之后就不该再有主密码框了,但「锁定 / 删除凭据文件」要在。
    ui.app.sync_status = Some(SyncStatus {
        store_locked: false,
        ready: true,
        problem: None,
        ..ui.app.sync_status.clone().unwrap()
    });
    render(&mut ui);
    render(&mut ui);
    fits(&ui, &sync_states);
    // 删除前的二次确认条也要能画出来。
    ui.app.sync_form.confirm_master_delete = true;
    render(&mut ui);
    render(&mut ui);
    fits(&ui, &sync_states);
    ui.app.sync_form.confirm_master_delete = false;

    ui.app.sync_status = Some(SyncStatus {
        store_kind: "session-only".into(),
        store_locked: false,
        ephemeral: true,
        ..sync_status_fixture()
    });
    render(&mut ui);
    render(&mut ui);
    fits(&ui, &sync_states);

    ui.app.sync_status = Some(SyncStatus {
        rclone: None,
        ephemeral: true,
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
    render(&mut ui);

    // 设置:Wine 状态没到 / 到了 / 有回话,快捷键没问到 / 问到了(含"授权了但没绑键")。
    show_tab(&mut ui, Tab::Settings);
    ui.app.sync_form.master_password.clear();
    render(&mut ui);
    ui.app.wine_status = Some(WineStatus {
        configured: Some("/prefixes/games".into()),
        default_prefix: "/home/user/.wine".into(),
        environment: None,
        detected: vec!["/home/user/.wine".into()],
    });
    ui.app.wine_msg = Some("已保存".into());
    render(&mut ui);
    ui.app.wine_msg = Some("保存失败: 这不是一个 wine prefix（缺少 drive_c）".into());
    render(&mut ui);
    ui.app.hotkeys = Some(HotkeyStatus {
        requested: true,
        ready: true,
        error: None,
        unbound: vec!["toggle-scale".into(), "fullscreen".into()],
        assign_hint: "系统设置 → 快捷键 → kotori".into(),
    });
    render(&mut ui);
    ui.app.hotkeys = Some(HotkeyStatus {
        requested: true,
        error: Some("An app id is required".into()),
        ..HotkeyStatus::default()
    });
    render(&mut ui);

    // 侧栏的连接状态:检测中 / 已连接 / 重试中 / 放弃,以及那条错误。
    for (connected, attempts) in [
        (Some(true), 0),
        (Some(false), 1),
        (Some(false), 99),
        (None, 0),
    ] {
        ui.app.daemon_connected = connected;
        ui.app.retry_attempts = attempts;
        ui.app.error = Some("boom".into());
        render(&mut ui);
    }

    // ── 回调 → 消息 ────────────────────────────────────────────────────
    // 上面证明"能画出来",这一段证明"点下去真的会到消息循环里":下标 → 枚举的
    // 映射(`tab` / 算法 / 存档形态 / 同步字段)如果错了,页面上完全看不出来。
    // 只碰纯状态的回调 —— 启动、保存、同步那些会真的去调守护进程。
    ui.app = App::new().0;
    ui.app.games = vec![ui_game()];
    render(&mut ui);
    crate::ui::driver::install_state(ui);
    let window = handle.upgrade().expect("窗口还在");
    crate::ui::wire::install_callbacks(&window);

    let assert_app = |check: &dyn Fn(&App)| with_ui(|ui| check(&ui.app));
    let draft = || with_ui(|ui| ui.app.draft.clone().expect("单游戏设置页要有草稿"));

    window.invoke_tab_changed(2);
    assert_app(&|app| assert_eq!(app.tab, Tab::Sync));
    window.invoke_tab_changed(99);
    assert_app(&|app| assert_eq!(app.tab, Tab::Games));

    window.invoke_search_changed("demo".into());
    assert_app(&|app| assert_eq!(app.search, "demo"));

    window.invoke_open_game("demo".into());
    assert_app(&|app| assert_eq!(app.selected.as_deref(), Some("demo")));
    assert!(window.get_game_open());

    // 算法下拉:下标 → `ScaleAlgorithm::ALL` 里的标签。
    window.invoke_algo_picked(1);
    assert_eq!(draft().algo, "Nis");
    window.invoke_algo_picked(99); // 越界就当没点
    assert_eq!(draft().algo, "Nis");

    window.invoke_sharpness_changed(4);
    assert_eq!(draft().sharpness, 4);
    window.invoke_game_dir_changed("/games/other".into());
    assert_eq!(draft().game_dir, "/games/other");
    window.invoke_ratio_changed("1.25".into());
    assert_eq!(draft().scale_ratio, "1.25");
    window.invoke_fullscreen_toggled(false);
    assert!(!draft().fullscreen);

    // 存档位置:加一行 → 换形态 → 删掉。
    window.invoke_add_save();
    assert_eq!(draft().save_paths.len(), 1);
    window.invoke_save_kind_picked(0, 2);
    assert_eq!(draft().save_paths[0].kind, "absolute");
    window.invoke_save_path_changed(0, "%APPDATA%\\Game".into());
    window.invoke_save_exclude_changed(0, "*.log".into());
    let entry = draft().save_paths[0].clone();
    assert_eq!(entry.path, "%APPDATA%\\Game");
    assert_eq!(entry.exclude, "*.log");
    window.invoke_remove_save(0);
    assert!(draft().save_paths.is_empty());

    // 「重置」= 草稿回到已存值 + 让页面重抄一份(种子必须 +1)。
    window.invoke_ratio_changed("9".into());
    assert_eq!(draft().scale_ratio, "9");
    let seed_before = window.get_detail_seed();
    window.invoke_reset();
    assert_eq!(draft().scale_ratio, "");
    assert_eq!(window.get_detail_seed(), seed_before + 1);

    window.invoke_delete_requested();
    assert_app(&|app| assert!(app.confirm_delete));
    window.invoke_delete_cancelled();
    assert_app(&|app| assert!(!app.confirm_delete));

    // 同步页:字段编号 → `SyncField`,8 是主密码(它不在 `[sync]` 里)。
    for (field, expected) in [
        (0, "endpoint"),
        (1, "bucket"),
        (2, "prefix"),
        (3, "keep_versions"),
        (4, "key_id"),
        (5, "app_key"),
        (6, "password"),
        (7, "password_again"),
    ] {
        window.invoke_sync_field(field, format!("v{field}").into());
        let form = with_ui(|ui| ui.app.sync_form.clone());
        let actual = match expected {
            "endpoint" => form.endpoint,
            "bucket" => form.bucket,
            "prefix" => form.prefix,
            "keep_versions" => form.keep_versions,
            "key_id" => form.key_id,
            "app_key" => form.app_key,
            "password" => form.password,
            _ => form.password_again,
        };
        assert_eq!(actual, format!("v{field}"), "字段 {field}");
    }
    window.invoke_sync_field(8, "master".into());
    assert_eq!(
        with_ui(|ui| ui.app.sync_form.master_password.clone()),
        "master"
    );

    // 加密开关要二次确认;确认后落在表单上,取消则什么都不改。
    window.invoke_sync_encryption_toggled(true);
    assert_eq!(window.get_sync_confirm_encryption(), 1);
    window.invoke_sync_confirm_encryption_clicked();
    assert!(with_ui(|ui| ui.app.sync_form.encryption));
    assert_eq!(window.get_sync_confirm_encryption(), 0);
    window.invoke_sync_encryption_toggled(false);
    assert_eq!(window.get_sync_confirm_encryption(), 2);
    window.invoke_sync_cancel_encryption_clicked();
    assert_eq!(window.get_sync_confirm_encryption(), 0);
    // 取消只是撤掉"待确认",已经确认过的那次改动还在(它要等「保存设置」才落盘)。
    assert!(with_ui(|ui| ui.app.sync_form.encryption));

    // 恢复要二次确认:`restore` 只记下"待确认",`restore_cancelled` 抹掉它。
    window.invoke_sync_restore("demo".into());
    assert_app(&|app| {
        assert_eq!(
            app.sync_restore_pending.as_ref().map(|(id, _)| id.as_str()),
            Some("demo")
        )
    });
    window.invoke_sync_restore_cancelled();
    assert_app(&|app| assert!(app.sync_restore_pending.is_none()));

    // 没先点"删除"就直接确认:什么都不该发生(防手滑)。
    window.invoke_sync_delete_master_confirmed();
    assert_app(&|app| assert!(!app.sync_form.busy));

    // 删除凭据文件:请求 → 待确认;取消 → 抹掉;确认 → 发出去。测试里没有 daemon,
    // 任务不会回包,所以只断言"待确认"被清掉、表单进入忙。
    window.invoke_sync_delete_master_requested();
    assert_app(&|app| assert!(app.sync_form.confirm_master_delete));
    window.invoke_sync_delete_master_cancelled();
    assert_app(&|app| assert!(!app.sync_form.confirm_master_delete));
    window.invoke_sync_delete_master_requested();
    window.invoke_sync_delete_master_confirmed();
    assert_app(&|app| {
        assert!(!app.sync_form.confirm_master_delete);
        assert!(app.sync_form.busy);
    });
    with_ui(|ui| ui.app.sync_form.busy = false);

    window.invoke_sync_lock_credentials();
    assert_app(&|app| assert!(app.sync_form.busy));
    with_ui(|ui| ui.app.sync_form.busy = false);

    window.invoke_wine_prefix_changed("/prefixes/mine".into());
    assert_app(&|app| assert_eq!(app.wine_prefix_input, "/prefixes/mine"));
    assert_app(&|app| assert!(app.wine_prefix_dirty));
}

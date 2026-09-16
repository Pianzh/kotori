//! 「设置」那一页的整页测试：Wine、后台服务、快捷键、环境检查，以及
//! "回调真的落到了 `App` 上"那一段。
//!
//! 从 `window_test/mod.rs` 拆出来（那边本来是 550 行的单个测试函数），但它**不是**
//! 独立的 `#[test]`：由主测试调用，共用同一个窗口。Slint 的测试后端在同一个进程里
//! 建第二个窗口，本地能过、CI 上不成立 —— 而这两段本来就是你中有我（回调那一段要
//! 在渲染过的窗口上 `invoke_*`）。

use super::super::*;
use super::{fits, show_tab};
use crate::ui::test_support::ui_game;

/// 渲染设置页的各种状态，然后驱动一遍回调，断言消息真的到了消息循环里。
///
/// ⚠ 窗口在调用方那边已经撑到 2600 高：`ElementHandle` 只看得见没被裁掉的部分。
pub(super) fn settings_page(mut ui: Ui) {
    let handle = ui.window.as_weak();
    render(&mut ui);

    // 设置:Wine 状态没到 / 到了 / 有回话,快捷键没问到 / 问到了(含"授权了但没绑键"),
    // 后台服务的四种样子(检测中 / 运行中 / 用户停掉的 / 有回话)。
    show_tab(&mut ui, Tab::Settings);
    ui.app.sync_form.master_password.clear();
    render(&mut ui);
    for (connected, paused) in [
        (Some(true), false),
        (Some(false), true),
        (Some(false), false),
    ] {
        ui.app.daemon_connected = connected;
        ui.app.daemon_paused = paused;
        render(&mut ui);
    }
    ui.app.daemon_connected = Some(true);
    ui.app.daemon_paused = false;
    ui.app.service_msg = Some("后台服务已停止（正在玩的游戏不受影响）".into());
    ui.app.service_busy = true;
    render(&mut ui);
    ui.app.service_msg = Some("启动失败: 守护进程未在 5 秒内就绪".into());
    ui.app.service_busy = false;
    render(&mut ui);
    // 后台服务那一行的控件最宽(状态字 + 两个按钮),也量一次:整页横向溢出
    // 就是从这里开始的(见 UI_GUIDE §7.14)。
    fits(&ui, &["CardRow"]);
    ui.app.service_msg = None;
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
    // 环境检查:三种状态各来一行,免得"缺少"那一行的红字没人看过。
    ui.app.environment = Some(Environment {
        distro: "Arch Linux".into(),
        ok: true,
        checks: vec![
            EnvCheck {
                title: "gamescope".into(),
                state: 0,
                state_label: "可用".into(),
                detail: "gamescope version 3.16.28 (gcc 16.2.1)".into(),
                impact: "启动游戏与缩放增强".into(),
                install: String::new(),
                required: true,
            },
            EnvCheck {
                title: "窗口尺寸控制".into(),
                state: 1,
                state_label: "有条件".into(),
                detail: "只在 KDE Plasma 上实现(平铺桌面里窗口尺寸是布局的事)".into(),
                impact: "改窗口尺寸会如实回「做不到」;滤镜与锐度仍然可用".into(),
                install: String::new(),
                required: false,
            },
            EnvCheck {
                title: "rclone".into(),
                state: 2,
                state_label: "缺少".into(),
                detail: "没找到".into(),
                impact: "没有它就没有云存档同步".into(),
                install: "sudo pacman -S rclone".into(),
                required: true,
            },
        ],
    });
    render(&mut ui);
    // 结果还没到的时候那一组也要能画(这时表是空的)。
    ui.app.environment = None;
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

    // 改一笔 = 挂一次防抖自动保存(这里只断言它真的排上了;写回由 app.rs 的单测盯)。
    let generation = with_ui(|ui| ui.app.autosave_generation);
    window.invoke_ratio_changed("9".into());
    assert_eq!(draft().scale_ratio, "9");
    assert_eq!(
        with_ui(|ui| ui.app.autosave_generation),
        generation + 1,
        "每改一笔都要排一次自动保存"
    );

    // 「重置」= 草稿回到已存值 + 作废挂着的那一笔 + 让页面重抄一份(种子必须 +1)。
    let seed_before = window.get_detail_seed();
    window.invoke_reset();
    assert_eq!(draft().scale_ratio, "");
    assert_eq!(window.get_detail_seed(), seed_before + 1);
    assert!(with_ui(|ui| ui.app.autosave_generation) > generation + 1);

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
    ] {
        window.invoke_sync_field(field, format!("v{field}").into());
        let form = with_ui(|ui| ui.app.sync_form.clone());
        let actual = match expected {
            "endpoint" => form.endpoint,
            "bucket" => form.bucket,
            "prefix" => form.prefix,
            "keep_versions" => form.keep_versions,
            "key_id" => form.key_id,
            _ => form.app_key,
        };
        assert_eq!(actual, format!("v{field}"), "字段 {field}");
    }
    // 6 是主密码:它不是 `[sync]` 里的设置项,所以不走 SyncField。
    window.invoke_sync_field(6, "master".into());
    assert_eq!(
        with_ui(|ui| ui.app.sync_form.master_password.clone()),
        "master"
    );

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

    // 「浏览…」:六个按钮都要接到消息循环上。接错(或漏接)在编译期看不出来 ——
    // 症状是"点了没反应",而对话框本身在测试里不会开,所以这里只断言请求发出去了。
    let browse = |which: i32| {
        with_ui(|ui| ui.app.picking = false);
        match which {
            0 => window.invoke_browse_new_game_dir(),
            1 => window.invoke_browse_new_exe(),
            2 => window.invoke_browse_game_dir(),
            3 => window.invoke_browse_exe(),
            4 => window.invoke_browse_save(0),
            _ => window.invoke_browse_wine_prefix(),
        }
        assert!(
            with_ui(|ui| ui.app.picking),
            "第 {which} 个「浏览…」按钮没有请对话框"
        );
    };
    for which in 0..6 {
        browse(which);
    }
    with_ui(|ui| ui.app.picking = false);

    // 后台服务:停止会立刻立起"别自动拉回来"的旗(请求本身不会跑 —— 测试里的
    // runtime 没人驱动,见 `Task`),启动则进入忙态。
    window.invoke_service_stop();
    assert_app(&|app| assert!(app.daemon_paused && app.service_busy));
    window.invoke_service_start();
    assert_app(&|app| assert!(app.service_busy));
}

//! 不开窗口的整页测试:用 Slint 的测试后端把真窗口建出来。
//!
//! 编译器管不到运行时,而 UI 的错误恰恰都在运行时:`Select.options[selected]` 越界、
//! `for` 到空数组、某个下标→枚举的映射写反、属性忘了填 —— 都是"翻到那一页才炸"。
//! 它顶替的是旧 UI 那份 `views_construct_for_every_tab_and_state`,并且额外把窗口回调
//! 挨个 `invoke_*` 一遍,断言消息真的落到了 `App` 上。
//!
//! 文件分工:本文件是共用的那两件事(`ui` 建窗口、`fits` 量宽度)加"游戏库 / 添加 /
//! 单游戏 / 云同步"四页;「设置」那一页在 `settings.rs`(拆开是因为它本来和这边挤在
//! 同一个 550 行的测试函数里)。

use std::rc::Rc;

use super::*;
use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

mod settings;

/// 建一个带测试后端的窗口。
fn ui() -> Ui {
    // ⚠ **不能用 `Once` 缓存这一次调用**（2026-09-16 被 CI 咬过一次）：测试后端是按
    // 线程注册的，缓存会让第二个测试线程拿不到它而回退到 winit —— 本地有 DISPLAY 时
    // winit 照样能建窗口（**假绿**），CI 上没有 DISPLAY 就直接炸在
    // "neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set"。
    // `hit_test` / `scroll_test` 也都是各自无条件调一次，照它们来。
    i_slint_backend_testing::init_no_event_loop();

    let window = AppWindow::new().expect("测试后端应该能建窗口");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("建一个 tokio runtime 只为拿 Handle");
    let games = Rc::new(VecModel::<GameItem>::default());
    let saves = Rc::new(VecModel::<SaveItem>::default());
    let process_rows = Rc::new(VecModel::<ProcessPickRow>::default());
    window.set_games(games.clone().into());
    window.set_saves(saves.clone().into());
    window
        .global::<ProcessPickerState>()
        .set_rows(process_rows.clone().into());

    Ui {
        app: App::new().0,
        window,
        runtime: runtime.handle().clone(),
        games,
        saves,
        pairing: Rc::new(VecModel::default()),
        cloud_rows: Rc::new(VecModel::default()),
        cloud_versions: Rc::new(VecModel::default()),
        process_rows,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    }
}

/// 切到某一页。`tab` 是**窗口自己的属性**(点导航栏时由 .slint 直接改),所以只改
/// `app.tab` 的话页面根本不会实例化 —— 那些"渲染过了"的断言会全部空跑(踩过)。
pub(super) fn show_tab(ui: &mut Ui, tab: Tab) {
    ui.app.tab = tab;
    ui.window.set_tab(match tab {
        Tab::Games => 0,
        Tab::Add => 1,
        Tab::Sync => 2,
        Tab::Settings => 3,
    });
}

/// 每一段都不能比窗口宽：输入框的 min-width 一旦等于 preferred-width，整页会被撑出去
/// （卡片被切、说明文字挤成一列竖字，见 UI_GUIDE §7.14）。
///
/// ⚠ 这条只能在整页测试里做：测试后端才拿得到元素几何。
pub(super) fn fits(ui: &Ui, type_names: &[&str]) {
    let window = &ui.window;
    let width = window.window().size().width as f32 / window.window().scale_factor();
    for type_name in type_names {
        let found: Vec<_> =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(window, type_name)
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
}

/// 现在这个窗口里有几个某种类型的元素。
///
/// 用来验"某一整块在某个条件下**根本不在树里**" —— 那种断言量不了几何,只能数。
pub(super) fn count(ui: &Ui, type_name: &str) -> usize {
    i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui.window, type_name).count()
}

#[test]
fn library_and_sync_pages_render_without_a_display() {
    let mut ui = ui();

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

    // ⚠ 「浏览…」挑回来的值有没有真的落到那个输入框里,**这里测不了**:Rust 推给窗口
    // 的是一个令牌,页面自己动手填(见 types.slint 的 `PathPick`),而测试后端读到的
    // `accessible_value()` 是**旧的** —— 拿早就存在的 `seed` 机制做对照实验也一样旧,
    // 所以那条断言会是假的。它靠快照看:`KOTORI_UI_PICK=<路径>`(见 snapshot.rs)。
    ui.app.draft = Some(Draft::from_game(&ui_game()));

    ui.app.confirm_delete = true;
    render(&mut ui);
    // 没有「保存」按钮了:这一行小字就是自动保存的全部汇报(进行中 / 成功 / 无改动 / 失败)。
    for (saving, message) in [
        (true, "保存中…"),
        (false, "已自动保存"),
        (false, "没有未保存的改动"),
        (false, "保存失败: 缩放比例必须是数字（当前 abc）"),
    ] {
        ui.app.saving = saving;
        ui.app
            .report_saved(message.to_string(), !message.starts_with("保存失败"));
        render(&mut ui);
    }
    ui.app.saved_msg = None;

    // 开着自动追踪的游戏(页面上"跟随的进程"那一行会显示出来)。
    ui.app.games = vec![UiGame {
        auto_watch: true,
        process_name: "game.exe".into(),
        ..ui_game()
    }];
    render(&mut ui);
    ui.app.games = vec![ui_game()];

    // 「从运行中的进程里挑」:浮层开 → 候选到 → 搜索 → 挑一个 → 添加游戏那三个框
    // 被填好(挑进程那条路的正面)。
    ui.app.process_picker.open();
    render(&mut ui);
    assert!(
        ui.window.global::<ProcessPickerState>().get_open(),
        "浮层该开着"
    );
    ui.app.process_picker.loaded(vec![ProcessRow {
        pid: 4321,
        name: "Game.exe".into(),
        title: "BLACKSOULS Ⅱ".into(),
        exe: "/games/blacksouls/Game.exe".into(),
    }]);
    render(&mut ui);
    assert_eq!(
        ui.window
            .global::<ProcessPickerState>()
            .get_rows()
            .row_count(),
        1
    );
    let _ = ui.app.update(Message::ProcessPicked(0));
    render(&mut ui);
    assert!(
        !ui.window.global::<ProcessPickerState>().get_open(),
        "挑完就该收起来"
    );
    assert_eq!(ui.app.new_exe, "/games/blacksouls/Game.exe");
    assert_eq!(
        ui.app.new_game_dir, "/games/blacksouls",
        "根目录跟着 exe 填"
    );
    assert_eq!(
        ui.app.new_name, "BLACKSOULS Ⅱ",
        "窗口标题比 exe 文件名认得出"
    );
    assert!(
        ui.app
            .create_msg
            .as_deref()
            .unwrap_or("")
            .contains("已按进程"),
        "{:?}",
        ui.app.create_msg
    );
    ui.app.new_exe.clear();
    ui.app.new_game_dir.clear();
    ui.app.new_name.clear();
    ui.app.create_msg = None;

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

    // 云同步:还没读到 → 一切就绪 → 待确认恢复 → 凭据文件锁着 →
    // 本机连密钥环都没有 → 没装 rclone 且有个存档位置解析不了。
    show_tab(&mut ui, Tab::Sync);
    render(&mut ui);
    ui.app.sync_status = Some(sync_status_fixture());
    ui.app.sync_form.apply(
        ui.app.sync_status.as_ref().unwrap(),
        &sync_payload()["settings"],
    );
    render(&mut ui);

    ui.app.sync_restore_pending = Some(("demo".into(), None));
    render(&mut ui);
    ui.app.sync_restore_pending = None;

    // 配对那一块:一条自动绑上的 + 一条要问的（两种最宽的形态：候选按钮最多）。
    ui.app.pairing = vec![
        PairingRow {
            cloud_key: "original-name".into(),
            cloud_id: "8f2c1234-0000-0000-0000-000000000000".into(),
            cloud_name: "Original Name".into(),
            machines: 2,
            state: PairingState::AutoBound,
            local_id: "renamed".into(),
            local_name: "Renamed".into(),
            evidence: "fingerprint".into(),
            choices: Vec::new(),
        },
        PairingRow {
            cloud_key: "other".into(),
            cloud_id: "1111".into(),
            cloud_name: "另一个名字很长的游戏（云端那一条）".into(),
            machines: 1,
            state: PairingState::Ask,
            local_id: String::new(),
            local_name: String::new(),
            evidence: String::new(),
            choices: vec![
                ("a".into(), "甲".into()),
                ("b".into(), "乙".into()),
                ("c".into(), "丙".into()),
            ],
        },
    ];
    ui.app.pairing_scanned = true;
    ui.app.pairing_msg = Some("云端 2 条身份，其中 1 条按 exe 指纹自动绑上了。".into());
    render(&mut ui);

    // 「云端存档」那一块:还没刷 → 列出来了(一款展开着看每一版)→ 空云 → 报错。
    // ⚠ 这一块的状态也在一个 Slint 全局里(照配对表),所以要把它那两份模型指到 `Ui`
    // 持有的这两份上 —— 否则 `push_model` 写的那一份窗口根本看不见,量出来是空树。
    ui.window
        .global::<CloudSavesBoard>()
        .set_rows(ui.cloud_rows.clone().into());
    ui.window
        .global::<CloudSavesBoard>()
        .set_versions(ui.cloud_versions.clone().into());

    // ⚠ `ElementHandle` 只看得见**没被裁掉**的部分(`ItemRc::is_visible` 判的是裁剪矩形):
    // 740 高的窗口里凭据组刚好在折线以下,而「云端存档」还在它下面 —— 所以量之前先把窗口
    // 撑高,否则查询会静默返回空,断言等于没写(这一点踩过一次)。
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2600.0));
    render(&mut ui);
    fits(&ui, &["SyncCloudSavesSection"]);

    ui.app.cloud = CloudBoard {
        rows: vec![
            CloudSaveRow {
                key: "demo".into(),
                versions: 2,
            },
            // 云端有、本机没有的那种:名字就是云端落点,而且会长得很难看 —— 正是
            // 要看它会不会把页面撑出去。
            CloudSaveRow {
                key: "only-on-the-other-machine-with-a-very-long-name".into(),
                versions: 0,
            },
        ],
        scanned: true,
        msg: Some("云端 2 款游戏，共 2 版存档。点一款看它每一版。".into()),
        open: Some("demo".into()),
        versions: vec![
            "20260910T090000Z".into(),
            "20260911T101500123Z-1a2b3c4d".into(),
        ],
        ..CloudBoard::default()
    };
    render(&mut ui);
    // 最宽的形态:一行长名字 + 展开着两个版本(人话时间 + 原始版本名)。
    fits(&ui, &["SyncCloudSavesSection"]);

    // 展开着但版本还在路上 / 云端一款都没有 / 列失败:三种都不许画成半截。
    ui.app.cloud.versions_loading = true;
    render(&mut ui);
    ui.app.cloud.versions_loading = false;
    ui.app.cloud = CloudBoard {
        scanned: true,
        msg: Some("云端还没有游戏。".into()),
        ..CloudBoard::default()
    };
    render(&mut ui);
    ui.app.cloud.ok = false;
    ui.app.cloud.msg = Some("列云端失败: 连不上桶".into());
    render(&mut ui);

    // 每一段都不能比窗口宽:输入框的 min-width 一旦等于 preferred-width,整页会被撑出去
    // (卡片被切、说明文字挤成一列竖字,见 UI_GUIDE §7.14)。凭据组是这一页最宽的一行
    // (说明 + 输入框 + 最多三个按钮),所以每个状态都量一次它。
    // ⚠ 这条只能在整页测试里做:测试后端才拿得到元素几何。
    let sync_states = [
        "SyncCredentialsGroup",
        "SyncPairingSection",
        "SyncCloudSavesSection",
        "CardRow",
    ];

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
            last: Some("× 2026-09-11T10:15 √".into()),
        }],
        ..sync_status_fixture()
    });
    render(&mut ui);

    // kopia 模式:引擎换掉之后是**另一组控件**(仓库密码 + 默认折叠的连接信息),
    // 而这一页最宽的一行正出在这里 —— 说明文字 + 输入框 + 两个按钮。
    // 折叠与展开两种样子都要量:连接信息展开后是一段长文本。
    ui.app.sync_status = Some(SyncStatus {
        engine: "kopia".into(),
        kopia: Some("/usr/bin/kopia".into()),
        kopia_prefix: "kotori/kopia".into(),
        ..sync_status_fixture()
    });
    ui.app.sync_form.engine = "kopia".into();
    render(&mut ui);
    fits(&ui, &["CardRow", "SyncKopiaGroup"]);
    ui.app.sync_form.connection_revealed = true;
    render(&mut ui);
    fits(&ui, &["CardRow", "SyncKopiaGroup"]);
    // kopia 没装:那一组要如实变红,而不是照常显示"可用"。
    ui.app.sync_status = Some(SyncStatus {
        kopia: None,
        ..ui.app.sync_status.clone().unwrap()
    });
    render(&mut ui);
    fits(&ui, &["CardRow", "SyncKopiaGroup"]);
    ui.app.sync_form.connection_revealed = false;
    ui.app.sync_form.engine = "rclone".into();

    // 存档行是单游戏页最宽的一行(形态 + 路径 + 「浏览…」+ 排除 + 删除),加上那颗按钮
    // 之后更挤了:也量一次(窗口这时已经是 2600 高,行才不会被裁掉)。
    ui.window.set_tab(0);
    ui.app.tab = Tab::Games;
    ui.app.selected = Some("demo".into());
    ui.window.set_game_open(true);
    ui.app.draft = Some(Draft {
        save_paths: kinds
            .iter()
            .map(|kind| SavePathDraft {
                kind: (*kind).to_string(),
                path: format!("/{kind}"),
                exclude: "*.log, cache/".into(),
            })
            .collect(),
        ..Draft::from_game(&ui_game())
    });
    render(&mut ui);
    fits(&ui, &["SaveRow"]);
    ui.window.set_game_open(false);
    ui.app.selected = None;
    ui.app.draft = None;

    // 设置页那一整段（含"回调 → 消息"）在 `settings` 里，共用这一个窗口。
    settings::settings_page(ui);
}

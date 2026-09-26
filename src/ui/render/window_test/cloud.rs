//! 「云端存档」页在无显示环境下的整页渲染：列表 → 详情 → 各种空/错状态 → 搜索。
//!
//! 从 `window_test/mod.rs` 拆出来（那边已经装不下这几页了）。它接手时窗口在
//! **「云同步」页**上，跑完把页面切回去 —— 后面那段量凭据组宽度的断言还等着它。

use super::*;

/// 造一份「云端存档」的状态。
///
/// ⚠ 不能写 `CloudState { … , ..CloudState::default() }`：模型里那两个字段是私有的，FRU 要求
/// 所有字段都看得见（编译期 E0451）。就地拿默认值改更省事，也不为此放宽可见性。
fn cloud_state(set: impl FnOnce(&mut CloudState)) -> CloudState {
    let mut cloud = CloudState::default();
    set(&mut cloud);
    cloud
}

pub(super) fn cloud_page(ui: &mut Ui) {
    // 「云端存档」是独立一页:还没读 → 读到了(一款展开着看每一版)→ 空云 → 报错。
    // ⚠ 这一页的状态在一个 Slint 全局里,所以要把它那两份模型指到 `Ui` 持有的这两份上
    // —— 否则 `push_model` 写的那一份窗口根本看不见,量出来是空树。
    show_tab(ui, Tab::Cloud);
    ui.window
        .global::<CloudBoard>()
        .set_rows(ui.cloud_rows.clone().into());
    ui.window
        .global::<CloudBoard>()
        .set_versions(ui.cloud_versions.clone().into());

    // ⚠ `ElementHandle` 只看得见**没被裁掉**的部分(`ItemRc::is_visible` 判的是裁剪矩形):
    // 740 高的窗口里凭据组刚好在折线以下,而这一页的内容也不止一屏 —— 所以量之前先把窗口
    // 撑高,否则查询会静默返回空,断言等于没写(这一点踩过一次)。
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2600.0));
    render(ui);
    fits(ui, &["CloudPage"]);

    ui.app.cloud = cloud_state(|cloud| {
        cloud.indexed = true;
        cloud.rows = vec![
            CloudGameRow {
                cloud_key: "original-name".into(),
                cloud_id: "8f2c1234-0000-0000-0000-000000000000".into(),
                name: "云端记下的游戏名（很长很长的那种）".into(),
                machines: 2,
                versions: 3,
                latest: Some("20260911T101500Z".into()),
                size: 4 * 1024 * 1024,
                // 云端有、本机没有的那种:exe 路径也得能显示出来(而且很长)。
                exe_paths: vec![r"D:\Games\Some Very Long Folder Name\game.exe".into()],
                local_id: "renamed".into(),
                local_name: "Renamed".into(),
                rejected: false,
            },
            CloudGameRow {
                cloud_key: "only-on-the-other-machine".into(),
                cloud_id: "1111".into(),
                name: "本机没有的那一款".into(),
                machines: 1,
                versions: 0,
                latest: None,
                size: 0,
                exe_paths: Vec::new(),
                local_id: String::new(),
                local_name: String::new(),
                rejected: true,
            },
        ];
        cloud.msg = Some("云端 2 款游戏。点一款看它每一版。".into());
    });
    render(ui);
    // 最宽的形态:一行长名字 + 用过的 exe 路径。
    fits(ui, &["CloudPage"]);

    // 点开一款:详情(meta + exe 路径 + 每一版)三种状态都要能画。
    ui.app.cloud.open("original-name");
    ui.app.cloud.versions_loading = false;
    ui.app.cloud.versions = vec![
        CloudVersionRow {
            name: "20260910T090000Z".into(),
            size: 128 * 1024,
            time: "2026-09-10T09:00:00Z".into(),
        },
        CloudVersionRow {
            name: "20260911T101500123Z-1a2b3c4d".into(),
            size: 4 * 1024 * 1024,
            time: "2026-09-11T10:15:00Z".into(),
        },
    ];
    render(ui);
    fits(ui, &["CloudPage"]);

    // 版本还在路上 / 一款都没有 / 索引还没建过 / 读失败:四种都不许画成半截。
    ui.app.cloud.versions_loading = true;
    render(ui);
    ui.app.cloud.versions_loading = false;
    ui.app.cloud.versions.clear();
    render(ui);
    ui.app.cloud.back();
    ui.app.cloud.indexed = false;
    ui.app.cloud.rows.clear();
    ui.app.cloud.msg = Some("桶里还没有这份索引。".into());
    render(ui);
    ui.app.cloud.ok = false;
    ui.app.cloud.msg = Some("读云端索引失败: 连不上桶".into());
    render(ui);
    fits(ui, &["CloudPage"]);

    // 搜索:命中一行 / 一行都不命中(那句"没有匹配的"要能画出来)。
    ui.app.cloud = cloud_state(|cloud| {
        cloud.indexed = true;
        cloud.rows = vec![CloudGameRow {
            cloud_key: "demo".into(),
            cloud_id: "2222".into(),
            name: "示例".into(),
            machines: 1,
            versions: 1,
            latest: Some("20260911T101500Z".into()),
            size: 4096,
            exe_paths: vec!["/games/demo/game.exe".into()],
            local_id: "demo".into(),
            local_name: "示例".into(),
            rejected: false,
        }];
        cloud.search = "zzz".into();
        cloud.msg = Some("云端 1 款游戏。".into());
    });
    render(ui);
    fits(ui, &["CloudPage"]);
    ui.app.cloud.search.clear();
    render(ui);

    // 跑完把页面切回「云同步」：下面那一大段（凭据、连接、kopia）都是量它的。
    show_tab(ui, Tab::Sync);
    render(ui);
}

/// 再下一层：**一个存档的管理页**（现在只有删除）。
///
/// 用户 2026-09-26 定的形态，所以这里钉三件事：这一页画得出来（宽不超窗、块与块之间没有
/// 被撑开的空档）、弹窗那几行字是 `model::confirm` 里那一条、以及删完它就自己收掉。
#[test]
fn the_archive_management_page_renders_and_deletes_through_the_shared_dialog() {
    let mut ui = ui();
    // ⚠ 这一页挂在「云端存档」那一页底下（`if CloudVersionBoard.open : CloudVersionPage`），
    //    所以得先切到那个标签 —— 不切的话整棵树里根本没有它。
    show_tab(&mut ui, Tab::Cloud);
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 1600.0));
    ui.app.cloud_version.opened(
        "示例游戏",
        "demo-key",
        &CloudVersionRow {
            name: "20260911T101500Z-1a2b3c4d".into(),
            size: 4 * 1024 * 1024,
            time: "2026-09-11T10:15:00Z".into(),
        },
    );
    render(&mut ui);

    let board = ui.window.global::<CloudVersionBoard>();
    assert!(board.get_open());
    assert_eq!(board.get_game_name(), "示例游戏");
    assert_eq!(board.get_version(), "20260911T101500Z-1a2b3c4d");
    assert_eq!(board.get_size_label(), "4.0 MiB");
    assert!(!board.get_confirm_open(), "没点删除之前不该有弹窗");
    assert!(!board.get_busy());
    fits(&ui, &["CloudVersionPage"]);

    // ⚠ 块与块之间不许有空档：这一页外层那个 `VerticalLayout` 里若有一层嵌套布局被拉伸，
    //    多余的高度就会摊在它里面（用户 2026-09-26 在另一页看见的正是这个：600 多 px 的
    //    空白）。这里用两个卡片组的间距当量尺 —— 正常情况下一百多 px，被撑开就是几百。
    let info =
        i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui.window, "InfoGroup")
            .next()
            .expect("这一版的信息卡");
    let action = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
        &ui.window,
        "ActionGroup",
    )
    .next()
    .expect("「删除这一版」那张卡");
    let gap = action.absolute_position().y - info.absolute_position().y;
    assert!(
        (0.0..300.0).contains(&gap),
        "信息卡到「删除这一版」那张卡之间是 {gap}px —— 这一页被布局摊开了",
    );

    // 点「删除这一版」：弹窗起来，字是共用的那一条（与单游戏设置那页说的一样）。
    let _ = ui.app.update(Message::CloudVersionDeleteRequested);
    render(&mut ui);
    let board = ui.window.global::<CloudVersionBoard>();
    assert!(board.get_confirm_open());
    assert_eq!(board.get_confirm_title(), "删掉云端的这一版？");
    assert_eq!(board.get_confirm_detail(), "20260911T101500Z-1a2b3c4d");
    assert!(board.get_confirm_danger());
    fits(&ui, &["CloudVersionPage"]);

    // 取消：什么都没有发生。
    let _ = ui.app.update(Message::CloudVersionCancelled);
    render(&mut ui);
    assert!(!ui.window.global::<CloudVersionBoard>().get_confirm_open());
    assert!(!ui.app.cloud_version.busy);

    // 确认后删失败：留在这一页，把原因写在页面上（不然用户以为删掉了）。
    let _ = ui.app.update(Message::CloudVersionDeleteRequested);
    let _ = ui.app.update(Message::CloudVersionConfirmed);
    render(&mut ui);
    assert!(ui.window.global::<CloudVersionBoard>().get_busy());
    let _ = ui
        .app
        .update(Message::CloudVersionDeleted(Err("云端没有这一版".into())));
    render(&mut ui);
    let board = ui.window.global::<CloudVersionBoard>();
    assert!(board.get_open() && !board.get_ok());
    assert!(
        board.get_message().starts_with("删除失败: "),
        "{}",
        board.get_message()
    );

    // 删成功：这一页自己收掉，结果那句话写在外层那一页上（用户被送回去时看得见）。
    let _ = ui.app.update(Message::CloudVersionDeleteRequested);
    let _ = ui.app.update(Message::CloudVersionConfirmed);
    let _ = ui.app.update(Message::CloudVersionDeleted(Ok(
        "已删掉云端那一版，这一款还剩 0 版".into(),
    )));
    render(&mut ui);
    assert!(!ui.window.global::<CloudVersionBoard>().get_open());
    assert!(
        ui.window
            .global::<CloudBoard>()
            .get_message()
            .contains("还剩 0 版"),
        "{}",
        ui.window.global::<CloudBoard>().get_message()
    );
}

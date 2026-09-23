//! 「添加游戏」页那块**云端匹配**在无显示环境下的渲染：正在问 / 唯一命中 / 云端没有 /
//! 还没建索引 / 问不成 / 多条候选 / 「不是这一款」/ 自己选（浮层）。
//!
//! 从 `window_test/mod.rs` 拆出来（那边是几页连起来的一条流水线，这一块自成一段）。
//! **每一态都要量宽度** —— 这一块里最宽的是"多条候选"那一堆按钮，浮层那张卡片则要和
//! 一个 620px 的列表一起放得下。
//!
//! ⚠ 状态在两个 Slint 全局里（`AddMatchBoard` / `CloudPickerState`），所以先把它们那两份
//! 模型指到 `Ui` 持有的那两份上：否则 `push_model` 写的那份窗口根本看不见，量出来是空树
//! （照 `cloud.rs`）。

use super::*;

/// 接管添加页（跑完把页面留在添加页上 —— 调用点在添加那一段的末尾）。
pub(super) fn add_match_states(ui: &mut Ui) {
    show_tab(ui, Tab::Add);
    ui.window
        .global::<AddMatchBoard>()
        .set_rows(ui.add_match_rows.clone().into());

    // ⚠ 这一页不止一屏。`ElementHandle` 只看得见**没被裁掉**的部分，所以先把窗口撑高，
    // 否则查询静默返回空、断言等于没写（同 `cloud.rs`）。
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2000.0));

    let exe = "/games/demo/game.exe";
    let row = |cloud_id: &str, name: &str| CloudGameRow {
        cloud_key: format!("key-{cloud_id}"),
        cloud_id: cloud_id.to_string(),
        name: name.to_string(),
        machines: 2,
        versions: 3,
        latest: Some("20260911T101500Z".to_string()),
        size: 4 * 1024 * 1024,
        exe_paths: vec![r"D:\Games\Some Very Long Folder Name\game.exe".to_string()],
        ..CloudGameRow::default()
    };

    // 正在问。
    ui.app.add_match.typing(exe);
    ui.app.add_match.asking(exe);
    render(ui);
    assert!(
        ui.window.global::<AddMatchBoard>().get_visible(),
        "问的时候要说话"
    );
    assert!(ui.window.global::<AddMatchBoard>().get_busy());
    fits(ui, &["AddPage"]);

    // 唯一命中：名字与版本数写在标题里，候选列表**不列**（不让人多点一下）。
    ui.app.add_match.loaded(
        exe,
        true,
        vec![row("c1", "云端记下的游戏名（很长很长的那种）")],
    );
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert_eq!(board.get_rows().row_count(), 0, "唯一命中不列候选");
    assert!(
        board.get_title().contains("云端已有这一款"),
        "{}",
        board.get_title()
    );
    assert!(board.get_ok());
    assert!(!board.get_declined());
    assert!(board.get_can_decline(), "有候选才有「不是这一款」");
    assert!(!board.get_picked());
    fits(ui, &["AddPage"]);

    // 云端没有这一款（索引建过、指纹对不上）：**没有候选** ⇒ 不给「不是这一款」
    // （用户 2026-09-23 点的：那时按下去什么也没改变，该给的是「自己选…」）。
    ui.app.add_match.asking(exe);
    ui.app.add_match.loaded(exe, true, Vec::new());
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert!(board.get_title().contains("云端没有这一款"));
    assert!(!board.get_can_decline(), "没有候选就不给「不是这一款」");
    fits(ui, &["AddPage"]);

    // 桶里还没建过索引：这是另一句话（提示去深度扫描），也**不给**那个按钮。
    ui.app.add_match.asking(exe);
    ui.app.add_match.loaded(exe, false, Vec::new());
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert!(
        board.get_title().contains("还没建索引"),
        "{}",
        board.get_title()
    );
    assert!(!board.get_can_decline());
    fits(ui, &["AddPage"]);

    // 没问成：灰着说，而且**不能**说成错误（它不影响添加）。
    ui.app.add_match.asking(exe);
    ui.app
        .add_match
        .failed(exe, "连不上桶: 网络不通".to_string());
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert!(!board.get_ok(), "问不成不该画成红字");
    assert!(board.get_visible());
    assert!(!board.get_can_decline(), "没问成时也没有候选");
    fits(ui, &["AddPage"]);

    // 多条候选（这一块最宽的形态）：列出来让人挑，绝不替他猜。
    ui.app.add_match.asking(exe);
    ui.app.add_match.loaded(
        exe,
        true,
        vec![row("c1", "一号候选"), row("c2", "二号候选（名字更长一点）")],
    );
    render(ui);
    assert_eq!(
        ui.window.global::<AddMatchBoard>().get_rows().row_count(),
        2
    );
    fits(ui, &["AddPage"]);

    // 挑中一条：选中标记要落到那一行上。
    ui.app.add_match.choose("c2");
    render(ui);
    let listed = ui.window.global::<AddMatchBoard>().get_rows();
    assert!(
        !listed.row_data(0).expect("候选该在").chosen,
        "没挑的那条不该有选中标记"
    );
    assert!(listed.row_data(1).expect("候选该在").chosen);
    fits(ui, &["AddPage"]);

    // 「不是这一款」→ 按钮换成"改主意"；再点回来。
    ui.app.add_match.decline();
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert!(board.get_declined());
    assert!(!board.get_can_decline(), "已经否掉了，别再给同一颗按钮");
    fits(ui, &["AddPage"]);
    ui.app.add_match.undo_decline();
    render(ui);
    assert!(!ui.window.global::<AddMatchBoard>().get_declined());

    // 自己选了一条（浮层那边挑完会走到这里）：标题换成"就绑这一条"，并给「改回自动」。
    ui.app.add_match.pick(row("c9", "用户自己认出来的那一条"));
    render(ui);
    let board = ui.window.global::<AddMatchBoard>();
    assert!(board.get_picked());
    assert!(
        board.get_title().contains("就绑这一条"),
        "{}",
        board.get_title()
    );
    fits(ui, &["AddPage"]);
    ui.app.add_match.clear_picked();
    render(ui);
    assert!(!ui.window.global::<AddMatchBoard>().get_picked());

    // 清场（添加成功、或者 exe 被清空）：整块连同候选一起消失。
    ui.app.add_match.reset();
    render(ui);
    assert!(!ui.window.global::<AddMatchBoard>().get_visible());
    assert_eq!(
        ui.window.global::<AddMatchBoard>().get_rows().row_count(),
        0
    );
    fits(ui, &["AddPage"]);
}

/// 「自己选…」那个浮层：读取中 → 有清单（含长名字）→ 搜索滤掉 → 挑一条 → 没索引 /
/// 读不成。跑完把浮层收起来（别影响后面那几页）。
pub(super) fn cloud_pick_states(ui: &mut Ui) {
    show_tab(ui, Tab::Add);
    ui.window
        .global::<CloudPickerState>()
        .set_rows(ui.cloud_pick_rows.clone().into());
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 1200.0));

    let row = |cloud_id: &str, name: &str| CloudGameRow {
        cloud_key: format!("key-{cloud_id}"),
        cloud_id: cloud_id.to_string(),
        name: name.to_string(),
        machines: 1,
        versions: 3,
        latest: Some("20260911T101500Z".to_string()),
        size: 4 * 1024 * 1024,
        local_id: String::new(),
        local_name: String::new(),
        rejected: false,
        exe_paths: vec![r"D:\Games\Some Very Long Folder Name\game.exe".to_string()],
    };

    // 打开：先是在读（这时候列表是空的，不许画成"云端没有游戏"）。
    ui.app.cloud_pick.open();
    render(ui);
    let board = ui.window.global::<CloudPickerState>();
    assert!(board.get_open(), "浮层该开着");
    assert!(board.get_loading());
    assert_eq!(board.get_rows().row_count(), 0);
    fits(ui, &["CloudPickerDialog"]);

    // 清单到了（名字很长的那种也要画得下）。
    ui.app.cloud_pick.loaded(
        true,
        vec![
            row("c1", "云端记下的游戏名（很长很长的那种）"),
            CloudGameRow {
                local_id: "renamed".into(),
                local_name: "本机这一款".into(),
                ..row("c2", "另一款")
            },
        ],
    );
    render(ui);
    let board = ui.window.global::<CloudPickerState>();
    assert_eq!(board.get_rows().row_count(), 2);
    assert!(!board.get_loading());
    assert_eq!(board.get_message(), "", "有货就别说话");
    fits(ui, &["CloudPickerDialog"]);

    // 搜索是**本地**过滤（不打网络），滤掉了多少要说出来。
    ui.app.cloud_pick.set_query("zzz".into());
    render(ui);
    let board = ui.window.global::<CloudPickerState>();
    assert_eq!(board.get_rows().row_count(), 0);
    assert!(
        board.get_message().contains("滤掉"),
        "{}",
        board.get_message()
    );
    fits(ui, &["CloudPickerDialog"]);

    // 挑一条：浮层收起，并且那条真的成了这一款要绑的目标。
    ui.app.cloud_pick.set_query(String::new());
    let picked = ui.app.cloud_pick.pick("c2").expect("这条在候选里");
    ui.app.add_match.pick(picked);
    render(ui);
    assert!(
        !ui.window.global::<CloudPickerState>().get_open(),
        "挑完要收"
    );
    assert!(ui.window.global::<AddMatchBoard>().get_picked());
    assert_eq!(
        ui.app.add_match.binding().map(|r| r.cloud_id.as_str()),
        Some("c2")
    );

    // 桶里还没建索引 / 读不成：两句不同的话，都不许画成"云端没有游戏"。
    ui.app.cloud_pick.open();
    ui.app.cloud_pick.loaded(false, Vec::new());
    render(ui);
    let board = ui.window.global::<CloudPickerState>();
    assert!(
        board.get_message().contains("深度扫描云端"),
        "{}",
        board.get_message()
    );
    fits(ui, &["CloudPickerDialog"]);

    ui.app.cloud_pick.open();
    ui.app.cloud_pick.failed("连不上桶".to_string());
    render(ui);
    let board = ui.window.global::<CloudPickerState>();
    assert!(
        board.get_message().contains("连不上桶"),
        "{}",
        board.get_message()
    );
    assert!(
        board.get_message().contains("照旧可以添加"),
        "{}",
        board.get_message()
    );
    fits(ui, &["CloudPickerDialog"]);

    // 收场：浮层关掉、匹配块清掉，后面的页面测试从干净状态继续。
    ui.app.cloud_pick.close();
    ui.app.add_match.reset();
    render(ui);
    assert!(!ui.window.global::<CloudPickerState>().get_open());
}

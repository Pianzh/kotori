//! 单游戏页那一页「这一款的云端存档」：身份那一行、云端那几版、以及四件事共用的二次确认弹窗。
//!
//! 状态在一个 Slint 全局里（`GameVersionsBoard`），所以先把它的模型指到 `Ui` 持有的那一份
//! 上：否则 `push_model` 写的那份窗口根本看不见，量出来是空树（照 `add.rs` / `cloud.rs`）。

use super::*;

fn version(name: &str, size: u64) -> CloudVersionRow {
    CloudVersionRow {
        name: name.to_string(),
        size,
        time: String::new(),
    }
}

/// 开着这一款、并把它那一条身份的版本页打开。
///
/// `bound` = 这一款绑没绑云端身份（身份那条注脚、页尾那两块"整款清理"都由它决定画不画）。
fn versions_page_with(ui: &mut Ui, rows: Vec<CloudVersionRow>, bound: bool) {
    show_tab(ui, Tab::Games);
    ui.window.set_game_open(true);
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 1600.0));
    ui.app.selected = Some("demo".into());
    ui.app.draft = Some(Draft::from_game(&ui_game()));
    if bound {
        // 夹具里这一款本来没绑；给它一条身份，"已绑定"那条路才走得通。
        let mut status = sync_status_fixture();
        for game in &mut status.games {
            if game.id == "demo" {
                game.cloud_id = "8d1f-e2b0".into();
                game.cloud_key = "demo-key".into();
                game.cloud_name = "Demo 的云端身份".into();
                game.cloud_versions = rows.len() as u64;
            }
        }
        ui.app.sync_status = Some(status);
    }
    // 模型指到 `Ui` 那一份（见文件头）。
    ui.window
        .global::<GameVersionsBoard>()
        .set_rows(ui.game_version_rows.clone().into());
    ui.app.versions.opened("demo", "demo-key");
    ui.app.versions.loaded(rows);
    render(ui);
}

fn versions_page(ui: &mut Ui, rows: Vec<CloudVersionRow>) {
    versions_page_with(ui, rows, true);
}

#[test]
fn the_versions_page_lists_the_cloud_copies_with_the_newest_first() {
    let mut ui = ui();
    versions_page(
        &mut ui,
        vec![
            version("20260901T000000Z", 1024),
            version("20260911T101500Z", 4096),
        ],
    );

    let board = ui.window.global::<GameVersionsBoard>();
    assert!(board.get_open(), "开着的那一款要把这一页画出来");
    assert!(!board.get_loading());
    assert_eq!(board.get_rows().row_count(), 2);
    // daemon 给的是"最旧在前"，这一页要最新在前。
    assert_eq!(
        board.get_rows().row_data(0).unwrap().name,
        "20260911T101500Z"
    );
    assert_eq!(
        board.get_rows().row_data(1).unwrap().name,
        "20260901T000000Z"
    );
    // 版本名那一列是原样的（桶里/CLI 看到的就是它），不是给人看的时间。
    assert_eq!(board.get_rows().row_data(0).unwrap().size_label, "4.0 KiB");
    assert!(!board.get_confirm_open(), "没点任何一颗按钮之前不该有弹窗");
    assert!(!board.get_busy());
    assert!(board.get_bound(), "夹具里这一款绑好了身份");
    fits(&ui, &["GameVersionsPage"]);
}

#[test]
fn a_replace_asks_first_and_cancelling_leaves_nothing_behind() {
    let mut ui = ui();
    versions_page(&mut ui, vec![version("20260911T101500Z", 4096)]);

    // 点「替换」：只是记下是哪一版，弹窗才起来。
    let _ = ui.app.update(Message::GameVersionsReplaceVersion(
        "20260911T101500Z".into(),
    ));
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert!(board.get_confirm_open());
    assert_eq!(board.get_confirm_title(), "用这一版替换本机的存档？");
    assert_eq!(board.get_confirm_detail(), "20260911T101500Z");
    assert_eq!(board.get_confirm_label(), "用这一版覆盖");
    assert!(!board.get_confirm_danger(), "替换不是删除，按钮不警示");
    assert!(!board.get_busy(), "确认之前不该在路上");
    fits(&ui, &["GameVersionsPage"]);

    // 取消：弹窗收掉，还是没动任何东西。
    let _ = ui.app.update(Message::GameVersionsCancelled);
    render(&mut ui);
    assert!(!ui.window.global::<GameVersionsBoard>().get_confirm_open());

    // 再来一次并确认：进入忙（那一版已经在路上了）。
    let _ = ui.app.update(Message::GameVersionsReplaceVersion(
        "20260911T101500Z".into(),
    ));
    let _ = ui.app.update(Message::GameVersionsConfirmed);
    render(&mut ui);
    assert!(ui.window.global::<GameVersionsBoard>().get_busy());
    assert!(!ui.window.global::<GameVersionsBoard>().get_confirm_open());
    assert_eq!(
        ui.app.versions.msg.as_deref(),
        Some("正在替换…"),
        "忙的时候那句话要说对是哪件事（正在替换）"
    );

    // 结果落在**这一页自己**那句话上（不是云同步页那句）。
    let _ = ui.app.update(Message::GameVersionsReplaced(Ok(
        "已用云端那一版覆盖本机存档".into(),
    )));
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_busy() && board.get_ok());
    assert!(
        board.get_message().contains("覆盖"),
        "{}",
        board.get_message()
    );
}

/// 删一版：弹窗换成"删除"那一套字，确认之后**列表里那一行当场少掉**。
#[test]
fn deleting_one_version_asks_first_then_drops_that_row() {
    let mut ui = ui();
    versions_page(
        &mut ui,
        vec![
            version("20260901T000000Z", 1024),
            version("20260911T101500Z", 4096),
        ],
    );

    let _ = ui.app.update(Message::GameVersionsDeleteVersion(
        "20260901T000000Z".into(),
    ));
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert!(board.get_confirm_open());
    assert_eq!(board.get_confirm_title(), "删掉云端的这一版？");
    assert_eq!(board.get_confirm_detail(), "20260901T000000Z");
    assert_eq!(board.get_confirm_label(), "删掉这一版");
    assert!(board.get_confirm_danger(), "删除要警示色");
    assert!(
        board.get_confirm_body().contains("本机的存档不动"),
        "{}",
        board.get_confirm_body()
    );
    fits(&ui, &["GameVersionsPage"]);

    let _ = ui.app.update(Message::GameVersionsConfirmed);
    render(&mut ui);
    assert!(ui.window.global::<GameVersionsBoard>().get_busy());
    assert_eq!(ui.app.versions.msg.as_deref(), Some("正在删除…"));

    let _ = ui.app.update(Message::GameVersionsDeleted(Ok(
        "已删掉云端那一版，这一款还剩 1 版".into(),
    )));
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_busy() && board.get_ok());
    assert_eq!(board.get_rows().row_count(), 1, "删掉的那一行要当场消失");
    assert_eq!(
        board.get_rows().row_data(0).unwrap().name,
        "20260911T101500Z"
    );
    assert!(
        board.get_message().contains("还剩 1 版"),
        "{}",
        board.get_message()
    );
}

/// 页尾那两颗"整款"按钮：清空这一款 / 抹掉整条词条。两个都没有具体版本，弹窗那一行明细
/// 改成"现在有几版"。
#[test]
fn the_two_whole_game_cleanups_share_the_same_dialog() {
    let mut ui = ui();
    versions_page(
        &mut ui,
        vec![
            version("20260901T000000Z", 1024),
            version("20260911T101500Z", 4096),
        ],
    );

    // ① 清空这一款的云端存档：身份留着。
    let _ = ui.app.update(Message::GameVersionsClearVersions);
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert_eq!(board.get_confirm_title(), "清空这一款的云端存档？");
    assert_eq!(
        board.get_confirm_detail(),
        "云端这一条身份现在有 2 版存档。"
    );
    assert_eq!(board.get_confirm_label(), "清空存档");
    assert!(board.get_confirm_danger());
    fits(&ui, &["GameVersionsPage"]);

    let _ = ui.app.update(Message::GameVersionsConfirmed);
    let _ = ui.app.update(Message::GameVersionsDeleted(Ok(
        "已清空这一款的云端存档（删掉 2 版），身份留着".into(),
    )));
    render(&mut ui);
    assert_eq!(
        ui.window
            .global::<GameVersionsBoard>()
            .get_rows()
            .row_count(),
        0
    );

    // ② 抹掉整条词条：话里要说清"本机还绑着它"，不然用户以为本机也解绑了。
    ui.app
        .versions
        .loaded(vec![version("20260901T000000Z", 1024)]);
    let _ = ui.app.update(Message::GameVersionsForgetIdentity);
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert_eq!(board.get_confirm_title(), "把这一款从云端抹掉？");
    assert_eq!(board.get_confirm_label(), "从云端抹掉");
    assert!(
        board.get_confirm_body().contains("本机还绑着它"),
        "{}",
        board.get_confirm_body()
    );
    fits(&ui, &["GameVersionsPage"]);

    let _ = ui.app.update(Message::GameVersionsConfirmed);
    let _ = ui.app.update(Message::GameVersionsDeleted(Ok(
        "已把这一款从云端抹掉（1 版存档连身份一起）".into(),
    )));
    render(&mut ui);
    assert_eq!(
        ui.window
            .global::<GameVersionsBoard>()
            .get_rows()
            .row_count(),
        0
    );
}

/// 删不成时列表**一行都不许少**（云端还在，界面就别说它没了）。
#[test]
fn a_failed_delete_leaves_the_list_alone_and_says_which_action_failed() {
    let mut ui = ui();
    versions_page(&mut ui, vec![version("20260901T000000Z", 1024)]);

    let _ = ui.app.update(Message::GameVersionsDeleteVersion(
        "20260901T000000Z".into(),
    ));
    let _ = ui.app.update(Message::GameVersionsConfirmed);
    let _ = ui.app.update(Message::GameVersionsDeleted(Err(
        "云端没有这一版: 20260901T000000Z".into(),
    )));
    render(&mut ui);

    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_ok());
    assert_eq!(board.get_rows().row_count(), 1, "没删成就不许动列表");
    assert!(
        board.get_message().starts_with("删除失败: "),
        "{}",
        board.get_message()
    );
}

/// 页尾那块「云端清理」必须**紧贴着**它的标题。
///
/// 用户 2026-09-26 看截图时一眼就是这个：标题在 358px、"清空存档"那张卡在 745px ——
/// 中间空了一大截。原因是那一块被包在一个会被外层布局拉伸的布局里（见页面里的注释）。
///
/// ⚠ 量的是"标题顶 → 卡片组顶"的距离，不是"标题底 → 卡片组顶"：被拉伸的是标题那个
/// `Text` 自己（文字画在盒子顶部，多出来的高度就成了视觉上的空白），拿盒子底去量反而
/// 算出 0，这条断言就白写了。
#[test]
fn the_cleanup_block_hugs_its_own_title() {
    let mut ui = ui();
    versions_page(&mut ui, vec![version("20260901T000000Z", 1024)]);

    let title = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
        &ui.window,
        "CleanupTitle",
    )
    .next()
    .expect("绑好了身份就该有「云端清理」那块");
    let group = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
        &ui.window,
        "CleanupGroup",
    )
    .next()
    .expect("「云端清理」下面那两张卡");
    let gap = group.absolute_position().y - title.absolute_position().y;
    assert!(
        (0.0..40.0).contains(&gap),
        "从「云端清理」标题到卡片组顶部是 {gap}px —— 布局把多余的高度摊到这一块里了",
    );
}

#[test]
fn a_game_with_nothing_in_the_cloud_says_so_instead_of_listing_nothing() {
    let mut ui = ui();
    versions_page(&mut ui, Vec::new());

    let board = ui.window.global::<GameVersionsBoard>();
    assert!(board.get_rows().row_count() == 0);
    assert!(!board.get_loading());
    // 一版都没有时那句空态（"这一条身份在云端还没有存档。"）由页面按 `bound` 挑，
    // 这里只钉住"列表是空的、而且没在转"。
    assert_eq!(board.get_message(), "");
    fits(&ui, &["GameVersionsPage"]);
}

/// 还没绑定身份：上面那条注脚要说"还没绑定"，而不是把空名字画出来。
#[test]
fn an_unbound_game_says_it_has_no_identity() {
    let mut ui = ui();
    versions_page_with(&mut ui, Vec::new(), false);

    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_bound());
    assert_eq!(board.get_identity(), "");
    fits(&ui, &["GameVersionsPage"]);
}

#[test]
fn a_failed_reading_says_why_on_this_page() {
    let mut ui = ui();
    versions_page(&mut ui, Vec::new());
    ui.app.versions.failed("连不上桶".into());
    render(&mut ui);

    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_ok());
    assert!(
        board.get_message().contains("连不上桶"),
        "{}",
        board.get_message()
    );
}

//! 单游戏页那一页「这一款的云端存档」：身份那两行、云端那几版、以及替换的二次确认。
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
fn versions_page(ui: &mut Ui, rows: Vec<CloudVersionRow>) {
    show_tab(ui, Tab::Games);
    ui.window.set_game_open(true);
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 1600.0));
    ui.app.selected = Some("demo".into());
    ui.app.draft = Some(Draft::from_game(&ui_game()));
    // 模型指到 `Ui` 那一份（见文件头）。
    ui.window
        .global::<GameVersionsBoard>()
        .set_rows(ui.game_version_rows.clone().into());
    ui.app.versions.opened("demo", "demo-key");
    ui.app.versions.loaded(rows);
    render(ui);
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
    assert_eq!(board.get_pending(), "", "没点「替换」之前不该有弹窗");
    assert!(!board.get_busy());
    fits(&ui, &["GameVersionsPage"]);
}

#[test]
fn a_replace_asks_first_and_cancelling_leaves_nothing_behind() {
    let mut ui = ui();
    versions_page(&mut ui, vec![version("20260911T101500Z", 4096)]);

    // 点「替换」：只是记下是哪一版，弹窗才起来。
    ui.app.versions.requested("20260911T101500Z");
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert_eq!(board.get_pending(), "20260911T101500Z");
    assert!(!board.get_busy(), "确认之前不该在路上");
    fits(&ui, &["GameVersionsPage"]);

    // 取消：弹窗收掉，还是没动任何东西。
    ui.app.versions.cancelled();
    render(&mut ui);
    assert_eq!(ui.window.global::<GameVersionsBoard>().get_pending(), "");

    // 再来一次并确认：进入忙（那一版已经在路上了）。
    ui.app.versions.requested("20260911T101500Z");
    let version = ui.app.versions.confirmed().expect("有待确认的那一版");
    assert_eq!(version, "20260911T101500Z");
    render(&mut ui);
    assert!(ui.window.global::<GameVersionsBoard>().get_busy());
    // 结果落在**这一页自己**那句话上（不是云同步页那句）。
    ui.app
        .versions
        .done(Ok("已用云端那一版覆盖本机存档".into()));
    render(&mut ui);
    let board = ui.window.global::<GameVersionsBoard>();
    assert!(!board.get_busy() && board.get_ok());
    assert!(
        board.get_message().contains("覆盖"),
        "{}",
        board.get_message()
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

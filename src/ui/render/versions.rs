//! 单游戏页那一页「这一款的云端存档」：身份那两行 + 云端那几版。
//!
//! 与 `render/cloud.rs`（那一页是整张云端清单）分开：这里的每一行都属于**本机这一款**，
//! 而且每行带一颗会覆盖本机存档的按钮。

use super::*;

pub(super) fn push_versions(ui: &mut Ui) {
    let app = &ui.app;
    let versions = &app.versions;
    let board = ui.window.global::<GameVersionsBoard>();

    push_bool(board.get_open(), versions.open, |v| board.set_open(v));
    push_str(
        board.get_game_name(),
        app.selected_game()
            .map(|game| game.name.as_str())
            .unwrap_or(""),
        |v| board.set_game_name(v),
    );

    // 身份那两行与单游戏页那块「当前绑定」**同一个函数**（`identity_label`），
    // 两处说法才不会各说各的。
    let label = app
        .selected_sync_game()
        .filter(|row| row.is_bound())
        .map(|row| {
            identity_label(
                &row.cloud_id,
                &row.cloud_key,
                &row.cloud_name,
                row.cloud_versions,
                &row.cloud_latest,
                row.cloud_size,
            )
        });
    let name = label
        .as_ref()
        .map(|label| label.name.clone())
        .unwrap_or_default();
    let summary = label
        .as_ref()
        .map(|label| label.summary.clone())
        .unwrap_or_default();
    push_bool(board.get_bound(), label.is_some(), |v| board.set_bound(v));
    push_str(board.get_identity(), &name, |v| board.set_identity(v));
    push_str(board.get_identity_summary(), &summary, |v| {
        board.set_identity_summary(v)
    });

    push_bool(board.get_loading(), versions.loading, |v| {
        board.set_loading(v)
    });
    push_str(
        board.get_message(),
        versions.msg.as_deref().unwrap_or(""),
        |v| board.set_message(v),
    );
    push_bool(board.get_ok(), versions.ok, |v| board.set_ok(v));
    push_bool(board.get_busy(), versions.busy, |v| board.set_busy(v));

    // 弹窗那几行字全在 Rust 里拼（见 `model::confirm`）：四种动作共用那一个弹窗组件，
    // 所以界面只管画，条件与文案一个字都不进 `.slint`。
    let values = ConfirmValues::of(versions.pending(), versions.rows.len());
    push_versions_confirm(&board, &values);

    let rows: Vec<CloudVersionItem> = versions
        .rows
        .iter()
        .map(|version| CloudVersionItem {
            label: version.label().into(),
            size_label: version.size_label().into(),
            name: version.name.clone().into(),
        })
        .collect();
    push_model(&ui.game_version_rows, rows);
}

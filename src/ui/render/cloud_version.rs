//! 「云端存档」页再下一层：**一个存档**的管理页（现在只有删除）。
//!
//! 与 `render/versions.rs`（那是"本机这一款 ↔ 云端"的那一页）分开：这一页只碰云端，所以
//! 没有任何"本机那一份"的东西可推，连"本机有没有这一款"都不必知道。

use super::*;

pub(super) fn push_cloud_version(ui: &mut Ui) {
    let state = &ui.app.cloud_version;
    let board = ui.window.global::<CloudVersionBoard>();

    push_bool(board.get_open(), state.open, |v| board.set_open(v));
    push_str(board.get_game_name(), &state.game_name, |v| {
        board.set_game_name(v)
    });
    push_str(board.get_version(), &state.version, |v| {
        board.set_version(v)
    });
    push_str(board.get_label(), &state.label, |v| board.set_label(v));
    push_str(board.get_size_label(), &state.size_label, |v| {
        board.set_size_label(v)
    });
    push_str(
        board.get_message(),
        state.msg.as_deref().unwrap_or(""),
        |v| board.set_message(v),
    );
    push_bool(board.get_ok(), state.ok, |v| board.set_ok(v));
    push_bool(board.get_busy(), state.busy, |v| board.set_busy(v));

    // 这一页只有一种动作，措辞还是 `model::confirm` 里那一条 —— 与单游戏设置那页点「删除」
    // 时说的话一字不差。
    let dialog = state.pending();
    let values = ConfirmValues::of(dialog.as_ref(), 0);
    push_cloud_version_confirm(&board, &values);
}

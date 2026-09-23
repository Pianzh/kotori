//! 「添加游戏」页:三个输入框的回灌,以及那块**云端匹配**。
//!
//! 匹配那一块的状态在一个 Slint 全局里（见 `pages/add.slint`）：页面只管画，窗口不必
//! 替它转发一串属性与回调。措辞（"还没建索引 / 云端没有 / 问不成"）都在 Rust 这边算好
//! —— 页面不做任何判断，所以这些分支有单测（见 `model::add`），窗口本身反而不用测。

use super::*;

pub(super) fn push_add(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    push_str(w.get_new_name(), &app.new_name, |v| w.set_new_name(v));
    push_str(w.get_new_game_dir(), &app.new_game_dir, |v| {
        w.set_new_game_dir(v)
    });
    push_str(w.get_new_exe(), &app.new_exe, |v| w.set_new_exe(v));
    push_bool(w.get_creating(), app.creating, |v| w.set_creating(v));
    let message = app.create_msg.clone().unwrap_or_default();
    let ok = message.starts_with("已添加");
    push_str(w.get_create_message(), &message, |v| {
        w.set_create_message(v)
    });
    push_bool(w.get_create_message_ok(), ok, |v| {
        w.set_create_message_ok(v)
    });

    push_add_match(ui);
    push_cloud_pick(ui);
}

/// 「云端匹配」那一块（exe 落定之后才有内容；**它从不挡住添加**）。
fn push_add_match(ui: &mut Ui) {
    let board = ui.window.global::<AddMatchBoard>();
    let m = &ui.app.add_match;

    push_bool(board.get_visible(), m.visible(), |v| board.set_visible(v));
    push_str(board.get_title(), &m.title(), |v| board.set_title(v));
    push_str(board.get_detail(), &m.detail(), |v| board.set_detail(v));
    // "没问成"灰着说 —— 那不是错误，也不影响添加（见 `model::add`）。
    let ok = !matches!(m.phase, MatchPhase::Failed(_));
    push_bool(board.get_ok(), ok, |v| board.set_ok(v));
    push_bool(board.get_busy(), m.busy(), |v| board.set_busy(v));
    push_bool(board.get_declined(), m.declined, |v| board.set_declined(v));
    push_bool(board.get_can_decline(), m.can_decline(), |v| {
        board.set_can_decline(v)
    });
    push_bool(board.get_picked(), m.has_picked(), |v| board.set_picked(v));

    // 只有指纹命中多条时才列出来让人挑；唯一命中直接写在标题里（不让人多点一下）。
    let rows: Vec<AddMatchItem> = if m.rows.len() > 1 {
        m.rows
            .iter()
            .map(|row| AddMatchItem {
                cloud_id: row.cloud_id.clone().into(),
                label: format!("《{}》 · {}", row.name, row.versions_label()).into(),
                detail: row.latest_label().into(),
                chosen: m.chosen.as_deref() == Some(row.cloud_id.as_str()),
            })
            .collect()
    } else {
        Vec::new()
    };
    push_model(&ui.add_match_rows, rows);
}

/// 「自己选…」那个浮层：开没开、搜索词、以及**过滤后**的云端清单。
///
/// 与「云端存档」页共用同一个 `sync.cloud_list` 回包，但**模型是分开的**：两处可能同时在
/// 窗口里，共用一个 `VecModel` 会互相覆盖（见 `driver::Ui` 里那两个字段）。
fn push_cloud_pick(ui: &mut Ui) {
    let board = ui.window.global::<CloudPickerState>();
    let pick = &ui.app.cloud_pick;

    push_bool(board.get_open(), pick.is_open(), |v| board.set_open(v));
    push_bool(board.get_loading(), pick.loading(), |v| {
        board.set_loading(v)
    });
    push_str(board.get_query(), pick.query(), |v| board.set_query(v));
    push_str(board.get_message(), &pick.message(), |v| {
        board.set_message(v)
    });

    let rows: Vec<CloudPickRow> = pick
        .rows()
        .iter()
        .map(|row| CloudPickRow {
            cloud_id: row.cloud_id.clone().into(),
            name: row.name.clone().into(),
            // 几版 + 最近一版（`latest_label` 一版都没有时是空串）。
            meta: match row.latest_label() {
                label if label.is_empty() => row.versions_label(),
                label => format!("{} · {}", row.versions_label(), label),
            }
            .into(),
            local_label: row.local_label().into(),
        })
        .collect();
    push_model(&ui.cloud_pick_rows, rows);
}

//! 「云端存档」页：一张表 + 一个详情。
//!
//! 状态在一个 Slint 全局里（见 `pages/cloud.slint`）：页面只管画，不转发属性。搜索是
//! **本地**过滤（`CloudBoard::visible`），所以敲字不打网络；真正读云端只有两颗按钮
//! ——「刷新」读索引（一次读），「深度扫描云端」读所有身份卡（慢，用户主动按）。

use super::*;

pub(super) fn push_cloud(ui: &mut Ui) {
    let cloud = &ui.app.cloud;
    let board = ui.window.global::<CloudBoard>();

    push_bool(board.get_indexed(), cloud.indexed, |v| board.set_indexed(v));
    push_bool(board.get_loading(), cloud.loading, |v| board.set_loading(v));
    push_bool(board.get_scanning(), cloud.scanning, |v| {
        board.set_scanning(v)
    });
    push_str(
        board.get_message(),
        cloud.msg.as_deref().unwrap_or(""),
        |v| board.set_message(v),
    );
    push_bool(board.get_ok(), cloud.ok, |v| board.set_ok(v));
    push_str(board.get_search(), &cloud.search, |v| board.set_search(v));
    push_bool(board.get_versions_loading(), cloud.versions_loading, |v| {
        board.set_versions_loading(v)
    });

    // ── 列表 ──
    let visible = cloud.visible();
    let rows: Vec<CloudGameItem> = visible
        .iter()
        .map(|row| CloudGameItem {
            key: row.cloud_key.clone().into(),
            name: row.name.clone().into(),
            meta: format!(
                "{} 台机器见过 · {} · {}",
                row.machines,
                crate::sync::cloud::short_id(&row.cloud_id, 8),
                row.versions_label()
            )
            .into(),
            versions_label: row.versions_label().into(),
            latest_label: row.latest_label().into(),
            local_label: row.local_label().into(),
        })
        .collect();
    let total = cloud.rows.len() as i32;
    push_model(&ui.cloud_rows, rows);
    push_int(board.get_total(), total, |v| board.set_total(v));

    // ── 详情 ──
    let opened = cloud.opened();
    push_bool(board.get_open(), opened.is_some(), |v| board.set_open(v));
    push_str(
        board.get_open_name(),
        opened.map(|row| row.name.as_str()).unwrap_or(""),
        |v| board.set_open_name(v),
    );
    push_str(
        board.get_open_meta(),
        &match opened {
            Some(row) => format!(
                "云端身份 {} · {} 台机器见过 · {}",
                crate::sync::cloud::short_id(&row.cloud_id, 8),
                row.machines,
                row.local_label()
            ),
            None => String::new(),
        },
        |v| board.set_open_meta(v),
    );
    push_str(
        board.get_open_exe(),
        &opened
            .map(|row| row.exe_paths.join("\n"))
            .unwrap_or_default(),
        |v| board.set_open_exe(v),
    );

    let versions: Vec<CloudVersionItem> = cloud
        .versions
        .iter()
        .map(|version| CloudVersionItem {
            label: version.label().into(),
            size_label: version.size_label().into(),
            name: version.name.clone().into(),
        })
        .collect();
    push_model(&ui.cloud_versions, versions);
}

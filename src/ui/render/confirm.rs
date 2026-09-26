//! 三个页面共用的那个二次确认弹窗：把措辞推下去。
//!
//! 弹窗本身是 `widgets/confirm-dialog.slint`；三个页面的全局带着同一批属性（名字一样），但
//! 生成出来的类型互不相同，所以只能一个全局一个函数。措辞住在 `model::confirm` —— 同一件事
//! 在三个入口上必须说同一句话。

use super::*;

/// 一次确认的全部内容（`open == false` = 不画弹窗）。
pub(super) struct ConfirmValues {
    open: bool,
    title: String,
    detail: String,
    body: String,
    label: String,
    danger: bool,
}

impl ConfirmValues {
    /// 按"现在要问的那件事"拼出这一套字。
    ///
    /// `count` = 这一页手上那份列表里有几版：两个"整款"动作靠它把"要删掉多少"说清楚
    /// （列表还没读回来时是 0，那一行就不画）。
    pub(super) fn of(pending: Option<&Confirmation>, count: usize) -> Self {
        match pending {
            Some(action) => Self {
                open: true,
                title: action.title().to_string(),
                detail: action.detail(count),
                body: action.body().to_string(),
                label: action.label().to_string(),
                danger: action.danger(),
            },
            None => Self {
                open: false,
                title: String::new(),
                detail: String::new(),
                body: String::new(),
                // 关着的时候这几个字没人看；给个顺眼的默认值，免得推一串空串过去。
                label: "确定".to_string(),
                danger: false,
            },
        }
    }
}

/// 「云端存档」页（列表 + 一款详情）。
pub(super) fn push_cloud_confirm(board: &CloudBoard, values: &ConfirmValues) {
    push_bool(board.get_confirm_open(), values.open, |v| {
        board.set_confirm_open(v)
    });
    push_str(board.get_confirm_title(), &values.title, |v| {
        board.set_confirm_title(v)
    });
    push_str(board.get_confirm_detail(), &values.detail, |v| {
        board.set_confirm_detail(v)
    });
    push_str(board.get_confirm_body(), &values.body, |v| {
        board.set_confirm_body(v)
    });
    push_str(board.get_confirm_label(), &values.label, |v| {
        board.set_confirm_label(v)
    });
    push_bool(board.get_confirm_danger(), values.danger, |v| {
        board.set_confirm_danger(v)
    });
}

/// 「云端存档」再下一层：一个存档的管理页。
pub(super) fn push_cloud_version_confirm(board: &CloudVersionBoard, values: &ConfirmValues) {
    push_bool(board.get_confirm_open(), values.open, |v| {
        board.set_confirm_open(v)
    });
    push_str(board.get_confirm_title(), &values.title, |v| {
        board.set_confirm_title(v)
    });
    push_str(board.get_confirm_detail(), &values.detail, |v| {
        board.set_confirm_detail(v)
    });
    push_str(board.get_confirm_body(), &values.body, |v| {
        board.set_confirm_body(v)
    });
    push_str(board.get_confirm_label(), &values.label, |v| {
        board.set_confirm_label(v)
    });
    push_bool(board.get_confirm_danger(), values.danger, |v| {
        board.set_confirm_danger(v)
    });
}

/// 单游戏设置那页的「这一款的云端存档」。
pub(super) fn push_versions_confirm(board: &GameVersionsBoard, values: &ConfirmValues) {
    push_bool(board.get_confirm_open(), values.open, |v| {
        board.set_confirm_open(v)
    });
    push_str(board.get_confirm_title(), &values.title, |v| {
        board.set_confirm_title(v)
    });
    push_str(board.get_confirm_detail(), &values.detail, |v| {
        board.set_confirm_detail(v)
    });
    push_str(board.get_confirm_body(), &values.body, |v| {
        board.set_confirm_body(v)
    });
    push_str(board.get_confirm_label(), &values.label, |v| {
        board.set_confirm_label(v)
    });
    push_bool(board.get_confirm_danger(), values.danger, |v| {
        board.set_confirm_danger(v)
    });
}

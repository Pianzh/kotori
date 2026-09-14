//! 状态 → 窗口。旧的 `view/` 层,现在只是一堆"先比再写"的属性赋值。
//!
//! 每页一个子模块,共用下面这几个小工具。两条规矩撑着可编辑控件:
//!
//! 1. **绝不写回一个已经在那里的值。** Slint 的 `in-out` 属性是输入控件自己的,
//!    从外面原样写回会把光标弹到行尾、重置用户正在敲的字段 —— 所以下面每个 push
//!    都先比较;用户敲下的那个字已经同时到了控件和消息循环,不需要再回灌一次。
//! 2. **整表只在形状变了时重建。** `set_vec` 会重建每一行,正在输入的那一行会因此
//!    丢掉焦点(见 `detail::push_saves`)。
//!
//! 这里(以及各子模块)只做"状态 → 属性",不含任何判断:措辞与条件都在 Rust 这边算好,
//! 页面的 `.slint` 只管显示 —— 所以这一层的纯函数都是有单测的,窗口本身反而不用测。

use super::*;
use slint::{Model, ModelRc, SharedString, VecModel};

use add::push_add;
use detail::push_detail;
use games::push_games;
use settings::push_settings;
use sync::push_sync;

mod add;
mod detail;
mod games;
mod settings;
mod sync;

#[cfg(test)]
mod hit_test;
#[cfg(test)]
mod scroll_test;
#[cfg(test)]
mod window_test;

pub(super) fn render(ui: &mut Ui) {
    push_shell(ui);
    push_browse(ui);
    push_games(ui);
    push_detail(ui);
    push_add(ui);
    push_sync(ui);
    push_settings(ui);
}
/// 「浏览…」:按钮能不能点,以及不能点时那行理由。
///
/// 一个窗口级属性而不是每页一份:能不能用只取决于**这台机器**上有没有文件对话框
/// (见 `crate::picker`),和哪个输入框无关。
fn push_browse(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;
    push_bool(w.get_browse_enabled(), app.can_browse(), |v| {
        w.set_browse_enabled(v)
    });
    push_str(w.get_path_hint(), &app.path_hint(), |v| w.set_path_hint(v));
}
fn push_shell(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    let (label, state) = connection_label(app);
    push_str(w.get_connection_label(), &label, |v| {
        w.set_connection_label(v)
    });
    push_int(w.get_connection_state(), state, |v| {
        w.set_connection_state(v)
    });
    push_bool(
        w.get_can_reconnect(),
        app.daemon_connected == Some(false),
        |v| w.set_can_reconnect(v),
    );
    push_str(
        w.get_error(),
        app.error.as_deref().unwrap_or_default(),
        |v| w.set_error(v),
    );
    push_bool(w.get_loading(), app.loading, |v| w.set_loading(v));
    push_bool(w.get_saving(), app.saving, |v| w.set_saving(v));
}
/// The sidebar's connection line, in the words the old GUI used.
///
/// Returns the text and the colour code the window wants: 0 检测中, 1 已连接,
/// 2 未连接. "Still retrying" is a different thing to tell a user than "gave
/// up" — with the retry counter spent, a "重连" button is the only way back.
fn connection_label(app: &App) -> (String, i32) {
    match app.daemon_connected {
        Some(true) => ("已连接".to_string(), 1),
        Some(false) if app.retry_attempts > 0 && app.retry_attempts <= MAX_AUTO_RETRIES => {
            ("未连接（重试中…）".to_string(), 2)
        }
        Some(false) => ("未连接".to_string(), 2),
        None => ("检测中…".to_string(), 0),
    }
}

/// Push only when the value differs — see rule 1 at the top of the file.
fn push_str(current: SharedString, value: &str, set: impl FnOnce(SharedString)) {
    if current.as_str() != value {
        set(value.into());
    }
}
fn push_bool(current: bool, value: bool, set: impl FnOnce(bool)) {
    if current != value {
        set(value);
    }
}
fn push_int(current: i32, value: i32, set: impl FnOnce(i32)) {
    if current != value {
        set(value);
    }
}
fn push_eq<T: PartialEq>(current: T, value: T, set: impl FnOnce(T)) {
    if current != value {
        set(value);
    }
}
/// Rebuild a list only when its contents actually changed.
fn push_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, want: Vec<T>) {
    if model.iter().collect::<Vec<T>>() != want {
        model.set_vec(want);
    }
}
/// A `&[&str]` constant as the window's string array.
fn options(values: impl IntoIterator<Item = &'static str>) -> ModelRc<SharedString> {
    strings(values.into_iter().map(SharedString::from).collect())
}
/// A list of strings as the window's string array (`ModelRc` has no `From<Vec>`,
/// it wants a model).
fn strings(items: Vec<SharedString>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_connection_line_tells_retrying_apart_from_giving_up() {
        let mut app = App::new().0;
        assert_eq!(connection_label(&app), ("检测中…".to_string(), 0));

        app.daemon_connected = Some(true);
        assert_eq!(connection_label(&app).1, 1);

        app.daemon_connected = Some(false);
        assert_eq!(connection_label(&app).0, "未连接");

        app.retry_attempts = 2;
        assert_eq!(connection_label(&app).0, "未连接（重试中…）");

        app.retry_attempts = MAX_AUTO_RETRIES + 1;
        assert_eq!(connection_label(&app).0, "未连接");
    }
}

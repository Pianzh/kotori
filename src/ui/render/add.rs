//! 「添加游戏」页:三个输入框的回灌。

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
}

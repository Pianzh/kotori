//! 设置页:Wine prefix 与全局快捷键状态。

use super::*;

pub(super) fn push_settings(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    push_str(w.get_wine_prefix(), &app.wine_prefix_input, |v| {
        w.set_wine_prefix(v)
    });
    let message = app.wine_msg.clone().unwrap_or_default();
    let ok = message.starts_with("已保存");
    push_str(w.get_wine_message(), &message, |v| w.set_wine_message(v));
    push_bool(w.get_wine_message_ok(), ok, |v| w.set_wine_message_ok(v));

    let wine = app.wine_status.as_ref();
    push_str(
        w.get_wine_effective(),
        wine.and_then(|status| status.configured.clone())
            .as_deref()
            .unwrap_or("自动探测"),
        |v| w.set_wine_effective(v),
    );
    push_str(
        w.get_wine_default(),
        wine.map(|status| status.default_prefix.as_str())
            .unwrap_or("读取中…"),
        |v| w.set_wine_default(v),
    );
    push_str(
        w.get_wine_environment(),
        wine.and_then(|status| status.environment.as_deref())
            .unwrap_or("未设置"),
        |v| w.set_wine_environment(v),
    );
    let detected: Vec<SharedString> = wine
        .map(|status| {
            status
                .detected
                .iter()
                .map(|path| path.as_str().into())
                .collect()
        })
        .unwrap_or_default();
    push_eq(w.get_wine_detected(), strings(detected), |v| {
        w.set_wine_detected(v)
    });

    let hotkeys = app.hotkeys.clone().unwrap_or_default();
    push_bool(w.get_hotkeys_loaded(), app.hotkeys.is_some(), |v| {
        w.set_hotkeys_loaded(v)
    });
    push_eq(
        w.get_hotkeys(),
        HotkeyState {
            requested: hotkeys.requested,
            ready: hotkeys.ready,
            error: hotkeys.error.unwrap_or_default().into(),
            unbound: hotkeys.unbound.join(" / ").into(),
            hint: hotkeys.assign_hint.into(),
        },
        |v| w.set_hotkeys(v),
    );
}

//! State → window. The old `view/` layer, as properties.
//!
//! Two rules keep this honest against the editable widgets:
//!
//! 1. **Never write a value that is already there.** Slint's `in-out` properties
//!    are what the input widgets own; assigning to one from outside moves the
//!    caret to the end of the line and can reset a field the user is typing in.
//!    Every push below compares first — so a keystroke, which has already
//!    reached the widget *and* the message loop, is never echoed back.
//! 2. **A whole list is only rebuilt when its shape changed.** `set_vec`
//!    recreates rows, which would take the focus away from the row being edited;
//!    see [`push_saves`].
//!
//! Everything here is a pure function of `App` (the only exception is the small
//! amount of bookkeeping in [`push_saves`]), which is why the conversion
//! functions at the bottom are unit-tested instead of the window.

use slint::{Model, ModelRc, SharedString, VecModel};

use super::*;

pub(super) fn render(ui: &mut Ui) {
    push_shell(ui);
    push_games(ui);
    push_detail(ui);
    push_add(ui);
    push_sync(ui);
    push_settings(ui);
}

// ── 外壳 ────────────────────────────────────────────────────────────────────

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

// ── 游戏库 ──────────────────────────────────────────────────────────────────

fn push_games(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    let visible: Vec<&UiGame> = app
        .games
        .iter()
        .filter(|game| matches_query(game, &app.search))
        .collect();

    push_str(w.get_search(), &app.search, |v| w.set_search(v));
    push_str(
        w.get_library_subtitle(),
        &library_subtitle(app, visible.len()),
        |v| w.set_library_subtitle(v),
    );
    push_str(
        w.get_library_empty(),
        &library_empty(app, visible.len()),
        |v| w.set_library_empty(v),
    );

    let items: Vec<GameItem> = visible.iter().map(|game| game_item(game, app)).collect();
    push_model(&ui.games, items);
}

/// "12 款游戏 · 点任意一行打开它的设置", or the same with a "showing n of m".
fn library_subtitle(app: &App, visible: usize) -> String {
    if app.search.trim().is_empty() {
        format!("{} 款游戏 · 点任意一行打开它的设置", app.games.len())
    } else {
        format!(
            "{} / {} 款 · 搜索「{}」",
            visible,
            app.games.len(),
            app.search.trim()
        )
    }
}

/// Why the list is empty, when it is. An empty string means "no need to say".
fn library_empty(app: &App, visible: usize) -> String {
    if app.games.is_empty() {
        if app.loading {
            "正在从守护进程加载游戏列表…".to_string()
        } else if app.error.is_some() {
            // 上面那条红条已经在解释了,不必说两遍。
            String::new()
        } else {
            "还没有游戏。切到「添加游戏」手动填一条。".to_string()
        }
    } else if visible == 0 {
        format!("没有匹配「{}」的游戏", app.search.trim())
    } else {
        String::new()
    }
}

/// One library row. `state` is 0 未运行 / 1 运行中 / 2 仅监视 — the last one
/// means a live session that is only being watched, which is a different thing
/// from a game *configured* as watch-only (that is `watch_only`).
fn game_item(game: &UiGame, app: &App) -> GameItem {
    let state = match app.running.get(&game.id) {
        Some(session) if session.watch_only => 2,
        Some(_) => 1,
        None => 0,
    };
    GameItem {
        id: game.id.clone().into(),
        name: game.name.clone().into(),
        exe: game.exe.clone().into(),
        ratio: ratio_label(game.scale_ratio).into(),
        state,
        watch_only: game.watch_only,
        launching: app.launching.as_deref() == Some(game.id.as_str()),
        process: game.process_name.clone().into(),
    }
}

/// The scaling ratio as the library shows it: bare digits, empty when unset.
///
/// No "×" and no "倍": glyph U+00D7 is intrinsically thin even in a bold face,
/// and U+2715 is missing from the fonts we use (see UI_GUIDE §7).
fn ratio_label(ratio: Option<f32>) -> String {
    ratio.map(|value| format!("{value}")).unwrap_or_default()
}

// ── 单个游戏 ────────────────────────────────────────────────────────────────

fn push_detail(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;

    push_eq(w.get_algo_options(), options(ScaleAlgorithm::ALL), |v| {
        w.set_algo_options(v)
    });
    push_eq(w.get_save_kinds(), options(SAVE_PATH_KINDS), |v| {
        w.set_save_kinds(v)
    });
    push_bool(w.get_confirm_delete(), app.confirm_delete, |v| {
        w.set_confirm_delete(v)
    });
    let (saved, saved_ok) = match &app.saved_msg {
        Some(message) => (message.clone(), !message.starts_with("保存失败")),
        None => (String::new(), true),
    };
    push_str(w.get_saved_message(), &saved, |v| w.set_saved_message(v));
    push_bool(w.get_saved_ok(), saved_ok, |v| w.set_saved_ok(v));
    push_bool(
        w.get_show_sharpness(),
        app.draft
            .as_ref()
            .is_some_and(|draft| matches!(draft.algo.as_str(), "Fsr" | "Nis")),
        |v| w.set_show_sharpness(v),
    );

    if let Some(game) = app.selected_game() {
        push_eq(w.get_game(), game_item(game, app), |v| w.set_game(v));
        // ⚠ 这份「已存值」只能来自库里的游戏,不能来自草稿:页面的可编辑副本是
        //    照它抄的,拿草稿去填等于每敲一个字就把输入框重置一次。
        push_eq(w.get_detail(), game_detail(game), |v| w.set_detail(v));
    }

    // 这个游戏的云存档状况:`sync.status` 里按 id 找那一行。
    let sync = app.selected_sync_game();
    push_str(w.get_detail_sync_line(), &sync_line(app, sync), |v| {
        w.set_detail_sync_line(v)
    });
    let can_act = app.sync_status.is_some()
        && sync.is_some_and(|row| row.locations > 0 && row.problem.is_none());
    let pending = app
        .sync_restore_pending
        .as_ref()
        .is_some_and(|(id, _)| app.selected.as_deref() == Some(id.as_str()));
    push_bool(w.get_detail_sync_can_act(), can_act, |v| w.set_detail_sync_can_act(v));
    push_bool(w.get_detail_sync_pending(), pending, |v| w.set_detail_sync_pending(v));
    push_bool(w.get_detail_sync_busy(), app.sync_form.busy, |v| {
        w.set_detail_sync_busy(v)
    });

    push_saves(ui);
}

impl App {
    /// `sync.status` 里属于当前这个游戏的那一行。
    pub(super) fn selected_sync_game(&self) -> Option<&SyncGameRow> {
        let id = self.selected.as_deref()?;
        self.sync_status
            .as_ref()?
            .games
            .iter()
            .find(|row| row.id == id)
    }
}

/// 单游戏设置页里「云存档」那行要说的话。
///
/// 措辞放在 Rust 里(而不是 .slint 里的三元表达式):它可以被测,而页面只管显示。
fn sync_line(app: &App, row: Option<&SyncGameRow>) -> String {
    if app.sync_status.is_none() {
        return "读取中…".to_string();
    }
    match row {
        None => "守护进程还没报这个游戏的存档位置".to_string(),
        Some(row) if row.problem.is_some() => {
            format!("⚠ {}", row.problem.as_deref().unwrap_or_default())
        }
        Some(row) if row.locations == 0 => {
            "还没有配置存档位置 —— 上面先加一条,同步才有东西可传。".to_string()
        }
        Some(row) => format!("{} 个存档位置 · {}", row.locations, row.last_label()),
    }
}

/// The stored profile, as the per-game page's "reset" basis.
fn game_detail(game: &UiGame) -> GameDetail {
    GameDetail {
        game_dir: game.game_dir.clone().into(),
        exe: game.exe.clone().into(),
        ratio: ratio_label(game.scale_ratio).into(),
        algo: ScaleAlgorithm::ALL
            .iter()
            .position(|label| *label == game.algo)
            .unwrap_or(0) as i32,
        // 滑块只有 0–5 档,存量配置里若有更大的值,先夹到能显示的范围内。
        sharpness: game.sharpness.min(5) as i32,
        internal_w: game.internal.0.to_string().into(),
        internal_h: game.internal.1.to_string().into(),
        output_w: game.output.0.to_string().into(),
        output_h: game.output.1.to_string().into(),
        fullscreen: game.fullscreen,
        framerate: game
            .framerate
            .map(|f| f.to_string())
            .unwrap_or_default()
            .into(),
    }
}

/// Keep the save-location model in step with the draft, without rebuilding it
/// while someone is typing in it.
///
/// The page owns the text of each row (a Slint input that is bound from outside
/// loses that binding the moment the user types); this model is only the
/// *initial* value of each row. So:
///
/// - entering a game, and adding or removing a location, change the shape of the
///   list ⇒ rebuild it from the draft (which has the latest text);
/// - changing a kind is one row's business ⇒ `set_row_data` on that row alone;
/// - text edits are **not** pushed back at all, or the row being typed in would
///   be recreated and lose the caret.
fn push_saves(ui: &mut Ui) {
    let app = &ui.app;
    let want: Vec<SaveItem> = app
        .draft
        .as_ref()
        .map(|draft| save_items(&draft.save_paths))
        .unwrap_or_default();

    if ui.saves_seed != ui.detail_seed || ui.saves_built.len() != want.len() {
        ui.saves.set_vec(want.clone());
        ui.saves_built = want;
        ui.saves_seed = ui.detail_seed;
        return;
    }

    for (index, item) in want.iter().enumerate() {
        if let Some(current) = ui.saves.row_data(index)
            && current.kind != item.kind
        {
            ui.saves.set_row_data(
                index,
                SaveItem {
                    kind: item.kind,
                    path: current.path,
                    exclude: current.exclude,
                    placeholder: item.placeholder.clone(),
                },
            );
            ui.saves_built[index].kind = item.kind;
        }
    }
}

/// 一份存档位置列表 → 视图结构。
fn save_items(entries: &[SavePathDraft]) -> Vec<SaveItem> {
    entries.iter().map(save_item).collect()
}

fn save_item(entry: &SavePathDraft) -> SaveItem {
    SaveItem {
        kind: SAVE_PATH_KINDS
            .iter()
            .position(|kind| *kind == entry.kind)
            .unwrap_or(0) as i32,
        path: entry.path.clone().into(),
        exclude: entry.exclude.clone().into(),
        placeholder: kind_placeholder(&entry.kind).into(),
    }
}

// ── 添加游戏 ────────────────────────────────────────────────────────────────

fn push_add(ui: &mut Ui) {
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

// ── 云同步 ──────────────────────────────────────────────────────────────────

fn push_sync(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;
    let form = &app.sync_form;
    let status = app.sync_status.as_ref();

    // ── 可编辑的一半 ──
    push_bool(w.get_sync_enabled(), form.enabled, |v| {
        w.set_sync_enabled(v)
    });
    push_str(w.get_sync_bucket(), &form.bucket, |v| w.set_sync_bucket(v));
    push_str(w.get_sync_prefix(), &form.prefix, |v| w.set_sync_prefix(v));
    push_str(w.get_sync_endpoint(), &form.endpoint, |v| {
        w.set_sync_endpoint(v)
    });
    push_str(w.get_sync_keep_versions(), &form.keep_versions, |v| {
        w.set_sync_keep_versions(v)
    });
    push_bool(w.get_sync_encryption(), form.encryption, |v| {
        w.set_sync_encryption(v)
    });
    push_int(
        w.get_sync_confirm_encryption(),
        match form.confirm_encryption {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        |v| w.set_sync_confirm_encryption(v),
    );
    push_str(w.get_sync_key_id(), &form.key_id, |v| w.set_sync_key_id(v));
    push_str(w.get_sync_app_key(), &form.app_key, |v| {
        w.set_sync_app_key(v)
    });
    push_str(w.get_sync_password(), &form.password, |v| {
        w.set_sync_password(v)
    });
    push_str(w.get_sync_password_again(), &form.password_again, |v| {
        w.set_sync_password_again(v)
    });
    push_str(w.get_sync_master_password(), &form.master_password, |v| {
        w.set_sync_master_password(v)
    });
    push_bool(w.get_sync_busy(), form.busy, |v| w.set_sync_busy(v));

    let message = form.msg.clone().unwrap_or_default();
    let ok = !message.contains("失败") && !message.contains("不一样") && !message.contains("请先");
    push_str(w.get_sync_message(), &message, |v| w.set_sync_message(v));
    push_bool(w.get_sync_message_ok(), ok, |v| w.set_sync_message_ok(v));

    // ── 只读的一半 ──
    push_bool(w.get_sync_loaded(), form.loaded, |v| w.set_sync_loaded(v));
    let (has_key_id, has_app_key) = match status {
        Some(status) => (
            status.has_secret("b2-key-id"),
            status.has_secret("b2-app-key"),
        ),
        None => (false, false),
    };
    let empty = SyncStatus::default();
    let status = status.unwrap_or(&empty);
    push_str(w.get_sync_remote(), &status.remote, |v| {
        w.set_sync_remote(v)
    });
    push_str(
        w.get_sync_rclone(),
        status.rclone.as_deref().unwrap_or_default(),
        |v| w.set_sync_rclone(v),
    );
    push_str(w.get_sync_keyring(), &status.keyring, |v| {
        w.set_sync_keyring(v)
    });
    push_bool(w.get_sync_ephemeral(), status.ephemeral, |v| {
        w.set_sync_ephemeral(v)
    });
    push_str(
        w.get_sync_keyring_hint(),
        crate::secrets::keyring_hint(),
        |v| w.set_sync_keyring_hint(v),
    );
    push_str(
        w.get_sync_problem(),
        status.problem.as_deref().unwrap_or_default(),
        |v| w.set_sync_problem(v),
    );
    push_bool(w.get_sync_ready(), status.ready, |v| w.set_sync_ready(v));
    push_int(
        w.get_sync_store_kind(),
        store_kind_index(&status.store_kind),
        |v| w.set_sync_store_kind(v),
    );
    push_bool(w.get_sync_store_locked(), status.store_locked, |v| {
        w.set_sync_store_locked(v)
    });
    push_str(w.get_sync_store_path(), &status.store_path, |v| {
        w.set_sync_store_path(v)
    });
    push_bool(
        w.get_sync_has_password(),
        status.has_secret("sync-password"),
        |v| w.set_sync_has_password(v),
    );
    push_bool(
        w.get_sync_has_credentials(),
        has_key_id || has_app_key,
        |v| w.set_sync_has_credentials(v),
    );
    push_str(
        w.get_sync_credentials_label(),
        &credentials_label(has_key_id, has_app_key),
        |v| w.set_sync_credentials_label(v),
    );
    push_str(w.get_sync_password_hint(), &status.password_hint, |v| {
        w.set_sync_password_hint(v)
    });
    push_str(
        w.get_sync_master_hint(),
        &format!("至少 {} 位,自己记得住就行", app.min_master_password()),
        |v| w.set_sync_master_hint(v),
    );

}

/// `sync.status` reports the credential store by name; the page wants a number
/// (it decides which of the three blocks to draw).
fn store_kind_index(kind: &str) -> i32 {
    match kind {
        "encrypted-file" => 1,
        "session-only" => 2,
        _ => 0,
    }
}

// ── 设置 ────────────────────────────────────────────────────────────────────

fn push_settings(ui: &mut Ui) {
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

// ── 小工具 ──────────────────────────────────────────────────────────────────

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
    use std::rc::Rc;

    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

    #[test]
    fn the_library_row_reports_a_live_session_and_the_watch_only_flag_apart() {
        let game = ui_game();
        let mut app = App::new().0;

        // 只是"配置成仅监视",不是"正在被监视"。
        let mut configured = game.clone();
        configured.watch_only = true;
        assert_eq!(game_item(&configured, &app).state, 0);

        app.running.insert(
            game.id.clone(),
            SessionInfo {
                session_id: "s1".into(),
                watch_only: true,
            },
        );
        assert_eq!(game_item(&configured, &app).state, 2);

        app.running.insert(
            game.id.clone(),
            SessionInfo {
                session_id: "s2".into(),
                watch_only: false,
            },
        );
        assert_eq!(game_item(&game, &app).state, 1);

        app.launching = Some(game.id.clone());
        assert!(game_item(&game, &app).launching);
    }

    #[test]
    fn the_ratio_column_shows_digits_and_nothing_else() {
        assert_eq!(ratio_label(Some(1.75)), "1.75");
        assert_eq!(ratio_label(Some(2.0)), "2");
        assert_eq!(ratio_label(None), "");
    }

    #[test]
    fn the_stored_profile_is_what_the_page_resets_to() {
        let mut game = ui_game();
        game.scale_ratio = Some(1.25);
        let detail = game_detail(&game);
        assert_eq!(detail.algo, 0);
        assert_eq!(detail.ratio, "1.25");
        assert_eq!(detail.internal_w, "1280");
        assert_eq!(detail.output_h, "1440");
        assert_eq!(detail.sharpness, 2);
        assert!(detail.fullscreen);

        // 存量里若有滑块放不下的锐度,只夹显示值,不改存的值。
        game.sharpness = 9;
        assert_eq!(game_detail(&game).sharpness, 5);
        assert_eq!(game.sharpness, 9);
    }

    #[test]
    fn an_unknown_algorithm_falls_back_to_the_first_option() {
        let mut game = ui_game();
        game.algo = "Lanczos".into();
        assert_eq!(game_detail(&game).algo, 0);
    }

    #[test]
    fn the_empty_library_says_why_it_is_empty() {
        let mut app = App::new().0;
        app.loading = true;
        assert!(library_empty(&app, 0).contains("加载"));

        app.loading = false;
        assert!(library_empty(&app, 0).contains("添加游戏"));

        // 有错误时那条红条已经在解释了,列表不必再说一遍。
        app.error = Some("连接被拒绝".into());
        assert_eq!(library_empty(&app, 0), "");

        app.error = None;
        app.games = vec![ui_game()];
        app.search = "nothing".into();
        assert!(library_empty(&app, 0).contains("nothing"));
        assert_eq!(library_empty(&app, 1), "");
    }

    #[test]
    fn the_subtitle_counts_what_is_shown_and_what_exists() {
        let mut app = App::new().0;
        app.games = vec![ui_game(), ui_game()];
        assert!(library_subtitle(&app, 2).contains("2 款游戏"));

        app.search = " demo ".into();
        let subtitle = library_subtitle(&app, 1);
        assert!(subtitle.contains("1 / 2"), "{subtitle}");
        assert!(subtitle.contains("demo"), "{subtitle}");
    }

    /// 建一个真窗口,把所有页面和关键状态渲染一遍。
    ///
    /// 这是旧 UI 那份 `views_construct_for_every_tab_and_state` 的替身,用的是 Slint
    /// 自带的测试后端 —— 不需要显示器,也不需要事件循环。它值钱的地方在于:编译器管不到
    /// 运行时,`Select.options[selected]` 越界、`for` 到空数组、某个属性忘了填,
    /// 全都是"翻到那一页才炸",而我没法自己看屏幕。
    #[test]
    fn every_page_renders_without_a_display() {
        i_slint_backend_testing::init_no_event_loop();

        let window = AppWindow::new().expect("测试后端应该能建窗口");
        // `window` 马上要搬进 Ui(句柄不是 Clone 的),回调那一段再从弱引用借回来。
        let handle = window.as_weak();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建一个 tokio runtime 只为拿 Handle");
        let games = Rc::new(VecModel::<GameItem>::default());
        let saves = Rc::new(VecModel::<SaveItem>::default());
        window.set_games(games.clone().into());
        window.set_saves(saves.clone().into());

        let mut ui = Ui {
            app: App::new().0,
            window,
            runtime: runtime.handle().clone(),
            games,
            saves,
            saves_built: Vec::new(),
            saves_seed: 0,
            detail_seed: 0,
        };

        // 游戏库:空列表 → 有列表 → 搜索命中 / 不命中 → 运行中 / 仅监视 → 启动中。
        render(&mut ui);
        ui.app.games = vec![ui_game()];
        ui.app.search = "demo".into();
        render(&mut ui);
        ui.app.search = "zzz".into();
        render(&mut ui);
        ui.app.search.clear();
        ui.app.launching = Some("demo".into());
        render(&mut ui);
        for watch_only in [false, true] {
            ui.app.running.insert(
                "demo".into(),
                SessionInfo {
                    session_id: "s".into(),
                    watch_only,
                },
            );
            render(&mut ui);
        }
        ui.app.running.clear();
        ui.app.launching = None;

        // 单游戏设置页:先让 render 把下拉的选项推上去,再打开页面 —— 顺序反了就是
        // 拿空数组当下拉,真机上会看到一行报错。
        ui.app.selected = Some("demo".into());
        ui.app.draft = Some(Draft::from_game(&ui_game()));
        render(&mut ui);
        ui.window.set_game_open(true);
        render(&mut ui);

        // 三种形态的存档位置各一条(下拉、占位符、删除按钮都要能用)。
        let kinds = ["windows", "relative", "absolute"];
        ui.app.draft = Some(Draft {
            save_paths: kinds
                .iter()
                .map(|kind| SavePathDraft {
                    kind: (*kind).to_string(),
                    path: format!("/{kind}"),
                    exclude: "*.log".into(),
                })
                .collect(),
            ..Draft::from_game(&ui_game())
        });
        render(&mut ui);

        ui.app.confirm_delete = true;
        render(&mut ui);
        ui.app.saved_msg = Some("已保存并通知守护进程".into());
        render(&mut ui);

        // 仅观测的游戏(页面上会多一节,内容来自 process_name)。
        ui.app.games = vec![UiGame {
            watch_only: true,
            process_name: "game.exe".into(),
            ..ui_game()
        }];
        render(&mut ui);
        ui.app.games = vec![ui_game()];

        // 添加游戏:空表单 → 填好 → 有回话。
        ui.app.tab = Tab::Add;
        ui.app.selected = None;
        ui.app.draft = None;
        ui.app.confirm_delete = false;
        render(&mut ui);
        ui.app.new_name = "Demo".into();
        ui.app.new_game_dir = "/games/demo".into();
        ui.app.new_exe = "/games/demo/game.exe".into();
        render(&mut ui);
        ui.app.create_msg = Some("已添加（ID: demo）".into());
        render(&mut ui);

        // 云同步:还没读到 → 一切就绪 → 待确认加密 → 待确认恢复 → 凭据文件锁着 →
        // 本机连密钥环都没有 → 没装 rclone 且有个存档位置解析不了。
        ui.app.tab = Tab::Sync;
        render(&mut ui);
        ui.app.sync_status = Some(sync_status_fixture());
        ui.app.sync_form.apply(
            ui.app.sync_status.as_ref().unwrap(),
            &sync_payload()["settings"],
        );
        render(&mut ui);

        ui.app.sync_form.confirm_encryption = Some(true);
        render(&mut ui);
        ui.app.sync_form.confirm_encryption = None;
        ui.app.sync_restore_pending = Some(("demo".into(), None));
        render(&mut ui);
        ui.app.sync_restore_pending = None;

        ui.app.sync_status = Some(SyncStatus {
            store_kind: "encrypted-file".into(),
            store_locked: true,
            store_path: "/home/user/.config/kotori/secrets.json".into(),
            keyring: "主密码加密文件（已锁定）".into(),
            ready: false,
            problem: Some("凭据文件已锁定，请先用主密码解锁".into()),
            ..sync_status_fixture()
        });
        ui.app.sync_form.master_password = "typed".into();
        render(&mut ui);

        // 解锁之后就不该再有主密码框了。
        ui.app.sync_status = Some(SyncStatus {
            store_locked: false,
            ready: true,
            problem: None,
            ..ui.app.sync_status.clone().unwrap()
        });
        render(&mut ui);

        ui.app.sync_status = Some(SyncStatus {
            store_kind: "session-only".into(),
            store_locked: false,
            ephemeral: true,
            ..sync_status_fixture()
        });
        render(&mut ui);

        ui.app.sync_status = Some(SyncStatus {
            rclone: None,
            ephemeral: true,
            problem: Some("密钥环里还没有 B2 凭据".into()),
            games: vec![SyncGameRow {
                id: "demo".into(),
                name: "Demo".into(),
                locations: 0,
                problem: Some("存档位置「%NOPE%」解析不了".into()),
                last: Some("✗ 2026-09-11T10:15 ✓".into()),
            }],
            ..sync_status_fixture()
        });
        render(&mut ui);

        // 设置:Wine 状态没到 / 到了 / 有回话,快捷键没问到 / 问到了(含"授权了但没绑键")。
        ui.app.tab = Tab::Settings;
        ui.app.sync_form.master_password.clear();
        render(&mut ui);
        ui.app.wine_status = Some(WineStatus {
            configured: Some("/prefixes/games".into()),
            default_prefix: "/home/user/.wine".into(),
            environment: None,
            detected: vec!["/home/user/.wine".into()],
        });
        ui.app.wine_msg = Some("已保存".into());
        render(&mut ui);
        ui.app.wine_msg = Some("保存失败: 这不是一个 wine prefix（缺少 drive_c）".into());
        render(&mut ui);
        ui.app.hotkeys = Some(HotkeyStatus {
            requested: true,
            ready: true,
            error: None,
            unbound: vec!["toggle-scale".into(), "fullscreen".into()],
            assign_hint: "系统设置 → 快捷键 → kotori".into(),
        });
        render(&mut ui);
        ui.app.hotkeys = Some(HotkeyStatus {
            requested: true,
            error: Some("An app id is required".into()),
            ..HotkeyStatus::default()
        });
        render(&mut ui);

        // 侧栏的连接状态:检测中 / 已连接 / 重试中 / 放弃,以及那条错误。
        for (connected, attempts) in [
            (Some(true), 0),
            (Some(false), 1),
            (Some(false), 99),
            (None, 0),
        ] {
            ui.app.daemon_connected = connected;
            ui.app.retry_attempts = attempts;
            ui.app.error = Some("boom".into());
            render(&mut ui);
        }

        // ── 回调 → 消息 ────────────────────────────────────────────────────
        // 上面证明"能画出来",这一段证明"点下去真的会到消息循环里":下标 → 枚举的
        // 映射(`tab` / 算法 / 存档形态 / 同步字段)如果错了,页面上完全看不出来。
        // 只碰纯状态的回调 —— 启动、保存、同步那些会真的去调守护进程。
        ui.app = App::new().0;
        ui.app.games = vec![ui_game()];
        render(&mut ui);
        crate::ui::driver::install_state(ui);
        let window = handle.upgrade().expect("窗口还在");
        crate::ui::wire::install_callbacks(&window);

        let assert_app = |check: &dyn Fn(&App)| with_ui(|ui| check(&ui.app));
        let draft = || with_ui(|ui| ui.app.draft.clone().expect("单游戏设置页要有草稿"));

        window.invoke_tab_changed(2);
        assert_app(&|app| assert_eq!(app.tab, Tab::Sync));
        window.invoke_tab_changed(99);
        assert_app(&|app| assert_eq!(app.tab, Tab::Games));

        window.invoke_search_changed("demo".into());
        assert_app(&|app| assert_eq!(app.search, "demo"));

        window.invoke_open_game("demo".into());
        assert_app(&|app| assert_eq!(app.selected.as_deref(), Some("demo")));
        assert!(window.get_game_open());

        // 算法下拉:下标 → `ScaleAlgorithm::ALL` 里的标签。
        window.invoke_algo_picked(1);
        assert_eq!(draft().algo, "Nis");
        window.invoke_algo_picked(99); // 越界就当没点
        assert_eq!(draft().algo, "Nis");

        window.invoke_sharpness_changed(4);
        assert_eq!(draft().sharpness, 4);
        window.invoke_game_dir_changed("/games/other".into());
        assert_eq!(draft().game_dir, "/games/other");
        window.invoke_ratio_changed("1.25".into());
        assert_eq!(draft().scale_ratio, "1.25");
        window.invoke_fullscreen_toggled(false);
        assert!(!draft().fullscreen);

        // 存档位置:加一行 → 换形态 → 删掉。
        window.invoke_add_save();
        assert_eq!(draft().save_paths.len(), 1);
        window.invoke_save_kind_picked(0, 2);
        assert_eq!(draft().save_paths[0].kind, "absolute");
        window.invoke_save_path_changed(0, "%APPDATA%\\Game".into());
        window.invoke_save_exclude_changed(0, "*.log".into());
        let entry = draft().save_paths[0].clone();
        assert_eq!(entry.path, "%APPDATA%\\Game");
        assert_eq!(entry.exclude, "*.log");
        window.invoke_remove_save(0);
        assert!(draft().save_paths.is_empty());

        // 「重置」= 草稿回到已存值 + 让页面重抄一份(种子必须 +1)。
        window.invoke_ratio_changed("9".into());
        assert_eq!(draft().scale_ratio, "9");
        let seed_before = window.get_detail_seed();
        window.invoke_reset();
        assert_eq!(draft().scale_ratio, "");
        assert_eq!(window.get_detail_seed(), seed_before + 1);

        window.invoke_delete_requested();
        assert_app(&|app| assert!(app.confirm_delete));
        window.invoke_delete_cancelled();
        assert_app(&|app| assert!(!app.confirm_delete));

        // 同步页:字段编号 → `SyncField`,8 是主密码(它不在 `[sync]` 里)。
        for (field, expected) in [
            (0, "endpoint"),
            (1, "bucket"),
            (2, "prefix"),
            (3, "keep_versions"),
            (4, "key_id"),
            (5, "app_key"),
            (6, "password"),
            (7, "password_again"),
        ] {
            window.invoke_sync_field(field, format!("v{field}").into());
            let form = with_ui(|ui| ui.app.sync_form.clone());
            let actual = match expected {
                "endpoint" => form.endpoint,
                "bucket" => form.bucket,
                "prefix" => form.prefix,
                "keep_versions" => form.keep_versions,
                "key_id" => form.key_id,
                "app_key" => form.app_key,
                "password" => form.password,
                _ => form.password_again,
            };
            assert_eq!(actual, format!("v{field}"), "字段 {field}");
        }
        window.invoke_sync_field(8, "master".into());
        assert_eq!(
            with_ui(|ui| ui.app.sync_form.master_password.clone()),
            "master"
        );

        // 加密开关要二次确认;确认后落在表单上,取消则什么都不改。
        window.invoke_sync_encryption_toggled(true);
        assert_eq!(window.get_sync_confirm_encryption(), 1);
        window.invoke_sync_confirm_encryption_clicked();
        assert!(with_ui(|ui| ui.app.sync_form.encryption));
        assert_eq!(window.get_sync_confirm_encryption(), 0);
        window.invoke_sync_encryption_toggled(false);
        assert_eq!(window.get_sync_confirm_encryption(), 2);
        window.invoke_sync_cancel_encryption_clicked();
        assert_eq!(window.get_sync_confirm_encryption(), 0);
        // 取消只是撤掉"待确认",已经确认过的那次改动还在(它要等「保存设置」才落盘)。
        assert!(with_ui(|ui| ui.app.sync_form.encryption));

        // 恢复要二次确认:`restore` 只记下"待确认",`restore_cancelled` 抹掉它。
        window.invoke_sync_restore("demo".into());
        assert_app(&|app| {
            assert_eq!(
                app.sync_restore_pending.as_ref().map(|(id, _)| id.as_str()),
                Some("demo")
            )
        });
        window.invoke_sync_restore_cancelled();
        assert_app(&|app| assert!(app.sync_restore_pending.is_none()));

        window.invoke_wine_prefix_changed("/prefixes/mine".into());
        assert_app(&|app| assert_eq!(app.wine_prefix_input, "/prefixes/mine"));
        assert_app(&|app| assert!(app.wine_prefix_dirty));
    }

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

    #[test]
    fn the_credential_store_names_the_three_places_a_secret_can_live() {
        assert_eq!(store_kind_index("system"), 0);
        assert_eq!(store_kind_index("encrypted-file"), 1);
        assert_eq!(store_kind_index("session-only"), 2);
        // 不认识的答复按最坏情况算:当作系统密钥环,不吓唬用户。
        assert_eq!(store_kind_index(""), 0);
    }
}

//! 游戏库列表:副标题、空状态、每行的会话状态。

use super::*;

pub(super) fn push_games(ui: &mut Ui) {
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
pub(super) fn game_item(game: &UiGame, app: &App) -> GameItem {
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
pub(super) fn ratio_label(ratio: Option<f32>) -> String {
    ratio.map(|value| format!("{value}")).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::ui_game;
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
}

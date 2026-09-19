//! 单个游戏的设置页:已存值、存档位置列表、这一款自己的云存档状况。

use super::games::{game_item, ratio_label};
use super::*;

pub(super) fn push_detail(ui: &mut Ui) {
    // 「浏览…」刚选回来的值:单游戏页的输入框由**页面自己**持有(见文件头那条规矩),
    // 平时 Rust 不往里写 —— 但这一笔是用户自己按出来的,所以要把值推回去,否则他
    // 挑完了框里还是旧内容。
    push_picked_path(ui);

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
        Some(message) => (message.clone(), app.saved_ok),
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

    push_bool(
        w.get_game_direct_launch(),
        app.draft.as_ref().map(|d| d.direct_launch).unwrap_or(false),
        |v| w.set_game_direct_launch(v),
    );
    push_bool(
        w.get_game_watch_only(),
        app.draft.as_ref().map(|d| d.watch_only).unwrap_or(false),
        |v| w.set_game_watch_only(v),
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
    push_bool(w.get_detail_sync_can_act(), can_act, |v| {
        w.set_detail_sync_can_act(v)
    });
    push_bool(w.get_detail_sync_pending(), pending, |v| {
        w.set_detail_sync_pending(v)
    });
    push_bool(w.get_detail_sync_busy(), app.sync_form.busy, |v| {
        w.set_detail_sync_busy(v)
    });

    push_saves(ui);
}
/// 把「浏览…」选中的值推回页面(推完就丢掉:它只属于那一次点击)。
fn push_picked_path(ui: &mut Ui) {
    let Some((target, value)) = ui.app.picked_path.take() else {
        return;
    };
    let value = SharedString::from(value);
    match target {
        // 单游戏设置页的两个路径框由页面自己持有,窗口上没有属性可写 ⇒ 发一个令牌,
        // 页面收到后自己填那一个框(见 `types.slint` 的 `PathPick`)。
        PathTarget::GameDir | PathTarget::Exe => {
            let target = if target == PathTarget::GameDir { 0 } else { 1 };
            ui.app.pick_token = ui.app.pick_token.wrapping_add(1);
            let token = ui.app.pick_token;
            ui.window.set_path_pick(PathPick {
                token,
                target,
                value,
            });
        }
        PathTarget::SavePath(index) => {
            // 存档行的文本是**模型的初始值**(页面不往回写),所以改模型那一行就够了。
            if let Some(mut row) = ui.saves.row_data(index) {
                row.path = value.clone();
                ui.saves.set_row_data(index, row);
                if let Some(built) = ui.saves_built.get_mut(index) {
                    built.path = value;
                }
            }
        }
        // 其它目标由各自的页面从 App 状态读(添加游戏页、设置页与云同步页都是单向推的：
        // 那几个框是窗口属性,`render` 下一帧就会把新值写进去)。
        PathTarget::NewGameDir
        | PathTarget::NewExe
        | PathTarget::WinePrefix
        | PathTarget::RcloneBinary
        | PathTarget::KopiaBinary => {}
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
        // 游戏分辨率也留空＝自动(不传 -w/-h,由 gamescope 按自己的默认值画),
        // 所以空字符串是正常值。
        internal_w: game
            .internal
            .0
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
        internal_h: game
            .internal
            .1
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
        // 留空的输出尺寸＝自动(启动时按屏幕算),所以空字符串是正常值,不是漏填。
        output_w: game
            .output
            .0
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
        output_h: game
            .output
            .1
            .map(|v| v.to_string())
            .unwrap_or_default()
            .into(),
        fullscreen: game.fullscreen,
        framerate: game
            .framerate
            .map(|f| f.to_string())
            .unwrap_or_default()
            .into(),
        // 两项都是"一行文本":存的是 argv,"进页面抄一遍"时拼成一行,存回去时
        // 按空白再切开(见 `parse::scale::split_args`)。
        launch_args: game.launch_args.join(" ").into(),
        gamescope_args: game.gamescope_args.join(" ").into(),
    }
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::ui_game;
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

        // 空着的那两项在页面上就是空字符串(占位符写的是「自动」)。
        game.internal = (None, None);
        game.output = (None, None);
        let blank = game_detail(&game);
        assert_eq!(blank.internal_w, "");
        assert_eq!(blank.internal_h, "");
        assert_eq!(blank.output_w, "");
        assert_eq!(blank.output_h, "");

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
}

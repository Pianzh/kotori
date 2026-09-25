//! The bridge: one message loop, one window, both on the UI thread.
//!
//! `App::update` is synchronous and returns its effects as a [`Task`]; the
//! effects run on the tokio runtime that `main` already created, and each one
//! posts its result back with `slint::invoke_from_event_loop` — touching the
//! window from a worker thread is not allowed, so the message is what travels.
//!
//! The state lives in a thread local rather than being captured by the callbacks
//! because the completion closures must be `Send` (they cross back into the
//! event loop) while the window is not: only the message crosses, and the
//! closure looks the state up again on the other side.

use std::cell::RefCell;
use std::rc::Rc;

use slint::VecModel;

use super::*;

/// Everything the UI thread owns.
pub(super) struct Ui {
    pub(super) app: App,
    pub(super) window: AppWindow,
    /// Handle to the runtime `main` created; tasks are spawned onto it.
    pub(super) runtime: tokio::runtime::Handle,
    /// The three lists the window cannot own.
    ///
    /// Slint array properties are immutable — there is no `push` and no
    /// assignment by index — so anything the user can add to or remove from has
    /// to be held here and handed over as a model.
    pub(super) games: Rc<VecModel<GameItem>>,
    pub(super) saves: Rc<VecModel<SaveItem>>,
    /// 「云端存档」那一块的两张表：云端有哪几款、点开那一款有哪几版。
    pub(super) cloud_rows: Rc<VecModel<CloudGameItem>>,
    pub(super) cloud_versions: Rc<VecModel<CloudVersionItem>>,
    /// 单游戏页那一页「这一款的云端存档」里那几版。与上面那张**分开**：两处可能同时在
    /// 树里（那一页盖在单游戏页上），共用一个模型会互相覆盖（同 `cloud_pick_rows`）。
    pub(super) game_version_rows: Rc<VecModel<CloudVersionItem>>,
    /// 添加页那块云端匹配的候选（指纹命中多条时才有内容）。
    pub(super) add_match_rows: Rc<VecModel<AddMatchItem>>,
    /// 「自己选…」那个浮层里的云端清单（与上面那张表分开：两处同时在树里，共用一个
    /// 模型会互相覆盖）。
    pub(super) cloud_pick_rows: Rc<VecModel<CloudPickRow>>,
    /// 「从正在运行的进程里挑」浮层里那些行。整表只在内容变了时重建(见
    /// `render::push_process_picker`)。
    pub(super) process_rows: Rc<VecModel<ProcessPickRow>>,
    /// What the save list was last built from, so a rebuild can be told apart
    /// from an edit (see [`render::push_saves`]).
    pub(super) saves_built: Vec<SaveItem>,
    pub(super) saves_seed: i32,
    /// The value handed to the window as `detail-seed`; bumping it is how the
    /// per-game page is told to re-copy the stored profile.
    pub(super) detail_seed: i32,
}

impl Ui {
    /// Ask the per-game settings page to copy the stored profile again.
    ///
    /// Used when entering a game and when "重置" is pressed: the page owns the
    /// editable copy, so "copy it again" has to be a change it can observe.
    pub(super) fn reseed_detail(&mut self) {
        self.detail_seed = self.detail_seed.wrapping_add(1);
        self.window.set_detail_seed(self.detail_seed);
    }
}

thread_local! {
    /// The one UI state. `None` only until [`run`] has built the window.
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
}

/// Test-only: install a hand-built state, so a unit test can drive the callbacks
/// without going through [`run`] (which wants a display and an event loop).
#[cfg(test)]
pub(super) fn install_state(state: Ui) {
    UI.with(|slot| *slot.borrow_mut() = Some(Rc::new(RefCell::new(state))));
}

/// Run `f` with the UI state. Panics if called before [`run`] — a callback can
/// only fire once the window exists, so that would be a bug, not a condition.
pub(super) fn with_ui<T>(f: impl FnOnce(&mut Ui) -> T) -> T {
    let ui = UI
        .with(|slot| slot.borrow().clone())
        .expect("UI state is initialised in run()");
    f(&mut ui.borrow_mut())
}

/// Feed one message to the loop, show the result, then start its effects.
pub(super) fn dispatch(message: Message) {
    let task = with_ui(|ui| {
        let task = ui.app.update(message);
        render(ui);
        task
    });
    spawn(task);
}

/// Start every effect of a task, posting each result back to this thread.
fn spawn(task: Task<Message>) {
    let runtime = with_ui(|ui| ui.runtime.clone());
    for effect in task.into_effects() {
        runtime.spawn(async move {
            let message = effect.await;
            if let Err(error) = slint::invoke_from_event_loop(move || dispatch(message)) {
                // The window is gone, which is what closing it looks like from
                // here. Nothing left to update — say so and stop.
                tracing::debug!("窗口已关闭，丢弃一条回包: {error}");
            }
        });
    }
}

/// Build the window and hand the event loop to Slint.
pub(super) fn run() -> anyhow::Result<()> {
    let window = AppWindow::new()?;
    font::install_font(&window);
    // 回调先装好(`window` 还没被搬进 Ui),但这时不会有任何回调触发:
    // 事件循环还没开始跑。
    wire::install_callbacks(&window);
    let weak = window.as_weak();

    let runtime = tokio::runtime::Handle::current();
    let games = Rc::new(VecModel::<GameItem>::default());
    let saves = Rc::new(VecModel::<SaveItem>::default());
    let cloud_rows = Rc::new(VecModel::<CloudGameItem>::default());
    let cloud_versions = Rc::new(VecModel::<CloudVersionItem>::default());
    let game_version_rows = Rc::new(VecModel::<CloudVersionItem>::default());
    let add_match_rows = Rc::new(VecModel::<AddMatchItem>::default());
    let cloud_pick_rows = Rc::new(VecModel::<CloudPickRow>::default());
    let process_rows = Rc::new(VecModel::<ProcessPickRow>::default());
    window.set_games(games.clone().into());
    window.set_saves(saves.clone().into());
    window
        .global::<CloudBoard>()
        .set_rows(cloud_rows.clone().into());
    window
        .global::<CloudBoard>()
        .set_versions(cloud_versions.clone().into());
    window
        .global::<GameVersionsBoard>()
        .set_rows(game_version_rows.clone().into());
    window
        .global::<AddMatchBoard>()
        .set_rows(add_match_rows.clone().into());
    window
        .global::<CloudPickerState>()
        .set_rows(cloud_pick_rows.clone().into());
    window
        .global::<ProcessPickerState>()
        .set_rows(process_rows.clone().into());

    let (app, boot) = App::new();
    let ui = Rc::new(RefCell::new(Ui {
        app,
        window,
        runtime,
        games,
        saves,
        cloud_rows,
        cloud_versions,
        game_version_rows,
        add_match_rows,
        cloud_pick_rows,
        process_rows,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    }));
    UI.with(|slot| *slot.borrow_mut() = Some(ui));

    with_ui(render);
    spawn(boot);
    snapshot::install_capture(&weak);

    // `window` 已经搬进 Ui 了,要跑事件循环得从弱引用再借一次句柄。
    weak.upgrade().expect("窗口还活着（刚刚才建出来）").run()?;

    // Reaching this line means the event loop ended because every window was
    // closed — not because of a panic. Worth recording: "it just exited" is
    // ambiguous otherwise.
    tracing::info!("UI 退出：所有窗口已关闭（不是崩溃）");
    Ok(())
}

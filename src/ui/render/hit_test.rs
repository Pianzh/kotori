//! 用**真的指针事件**问一句:点下去,接住它的是不是该接住的那个控件。
//!
//! 这是几何断言(`window_test.rs` 里"没有哪一段比窗口宽")查不出来的那一类 bug:尺寸、
//! 位置全都对,只是**有东西盖在上面**。Slint 的命中测试从最上层往下走,后声明的兄弟
//! 先拿到事件 —— 一行 `TouchArea` 声明的位置错一格,里面的按钮就永远收不到点击。
//!
//! 踩过的就是这条:游戏库整行的 `TouchArea` 写在内容之后,把右边的「启动」盖住了,
//! 点启动没反应,反而进了详细设置。所以这里量两件事:
//!   ① 点按钮 ⇒ 按钮的动作发生,而且**没有**顺带进详细设置;
//!   ② 点行里的空白 ⇒ 进详细设置(整行可点这条不能被修没了)。
//!
//! ⚠ 事件坐标是**逻辑像素**,和 `ElementHandle` 报的几何同一套。

use std::rc::Rc;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{LogicalPosition, VecModel};

use super::*;
use crate::ui::test_support::ui_game;

/// 在窗口坐标上真按一下(按下 + 抬起,和鼠标点击同一条路径)。
fn click(window: &AppWindow, position: LogicalPosition) {
    let surface = window.window();
    surface.dispatch_event(WindowEvent::PointerMoved { position });
    surface.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    surface.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

/// 一个元素的正中心(拿它当点击坐标)。
fn center(element: &i_slint_backend_testing::ElementHandle) -> LogicalPosition {
    let origin = element.absolute_position();
    let size = element.size();
    LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0)
}

#[test]
fn a_click_lands_on_the_control_under_the_pointer() {
    i_slint_backend_testing::init_no_event_loop();

    let window = AppWindow::new().expect("测试后端应该能建窗口");
    let handle = window.as_weak();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("建一个 tokio runtime 只为拿 Handle");
    let games = Rc::new(VecModel::<GameItem>::default());
    let saves = Rc::new(VecModel::<SaveItem>::default());
    let process_rows = Rc::new(VecModel::<ProcessPickRow>::default());
    window.set_games(games.clone().into());
    window.set_saves(saves.clone().into());
    window
        .global::<ProcessPickerState>()
        .set_rows(process_rows.clone().into());

    let mut ui = Ui {
        app: App::new().0,
        window,
        runtime: runtime.handle().clone(),
        games,
        saves,
        cloud_rows: Rc::new(VecModel::default()),
        cloud_versions: Rc::new(VecModel::default()),
        game_version_rows: Rc::new(VecModel::default()),
        add_match_rows: Rc::new(VecModel::default()),
        cloud_pick_rows: Rc::new(VecModel::default()),
        process_rows,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    };
    ui.app.games = vec![ui_game()];
    render(&mut ui);
    crate::ui::driver::install_state(ui);
    let window = handle.upgrade().expect("窗口还在");
    crate::ui::wire::install_callbacks(&window);

    // 一行的两个目标:右边的按钮,和行里除按钮之外的空白。
    let row = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&window, "GameRow")
        .next()
        .expect("游戏库里应该有那一行");
    let row_top = row.absolute_position().y;
    let row_bottom = row_top + row.size().height;
    let button =
        i_slint_backend_testing::ElementHandle::find_by_element_type_name(&window, "AccentButton")
            .find(|candidate| {
                let top = candidate.absolute_position().y;
                // 只在没被裁掉、且落在这行里的那个按钮才算(别的页面不会同时实例化,但别赌)。
                top >= row_top && top <= row_bottom
            })
            .expect("那一行应该有「启动」按钮");

    // ⚠ 先把几何量成普通数字:下面每一次点击都可能让宿主**整表重建**游戏库
    //    (`push_games` 见内容变了就 `set_vec`),那时旧句柄就失效了 ——
    //    失效句柄的 `absolute_position()`/`size()` 都回 0,拿它算坐标会点到窗口角上。
    let row_left = row.absolute_position().x;
    let row_middle = row_top + row.size().height / 2.0;

    // ① 点按钮:动作要发生,而且不许顺带进详细设置。
    click(&window, center(&button));
    let launching = with_ui(|ui| ui.app.launching.clone());
    assert_eq!(
        launching.as_deref(),
        Some("demo"),
        "点「启动」必须真的去启动 —— 事件被整行的 TouchArea 抢走了"
    );
    assert!(!window.get_game_open(), "点「启动」不该顺带下钻到详细设置");

    // ② 再点一行里的空白(游戏名那一块):整行可点这条还在。
    click(&window, LogicalPosition::new(row_left + 40.0, row_middle));
    assert!(window.get_game_open(), "点整行还是要能进详细设置");
    assert_eq!(
        with_ui(|ui| ui.app.selected.clone()).as_deref(),
        Some("demo")
    );
}

/// 云存档组里那条**身份**是"整条可点 + 右边一颗按钮" —— 正是上面那条踩过的形状。
///
/// 所以两件事都要钉住:
///   ① 点条上的空白 ⇒ 进这一款的云端存档页(`versions.open`);
///   ② 点右边那颗「更改绑定…」⇒ 走它自己的动作,**不许**顺带进那一页。
#[test]
fn the_identity_row_opens_the_versions_page_but_its_button_still_wins() {
    i_slint_backend_testing::init_no_event_loop();

    let window = AppWindow::new().expect("测试后端应该能建窗口");
    let handle = window.as_weak();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("建一个 tokio runtime 只为拿 Handle");
    let games = Rc::new(VecModel::<GameItem>::default());
    let saves = Rc::new(VecModel::<SaveItem>::default());
    let process_rows = Rc::new(VecModel::<ProcessPickRow>::default());
    window.set_games(games.clone().into());
    window.set_saves(saves.clone().into());
    window
        .global::<ProcessPickerState>()
        .set_rows(process_rows.clone().into());

    let mut ui = Ui {
        app: App::new().0,
        window,
        runtime: runtime.handle().clone(),
        games,
        saves,
        cloud_rows: Rc::new(VecModel::default()),
        cloud_versions: Rc::new(VecModel::default()),
        game_version_rows: Rc::new(VecModel::default()),
        add_match_rows: Rc::new(VecModel::default()),
        cloud_pick_rows: Rc::new(VecModel::default()),
        process_rows,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    };
    ui.app.games = vec![ui_game()];
    // 进单游戏页,并把窗口撑高 —— 云存档组在页面最底下,`ElementHandle` 只看得到
    // 没被裁掉的部分(同 `window_test` 里那段)。
    ui.app.selected = Some("demo".into());
    ui.app.draft = Some(Draft::from_game(&ui_game()));
    ui.window.set_game_open(true);
    ui.window
        .window()
        .set_size(slint::LogicalSize::new(1120.0, 2600.0));
    render(&mut ui);
    crate::ui::driver::install_state(ui);
    let window = handle.upgrade().expect("窗口还在");
    crate::ui::wire::install_callbacks(&window);

    // 量一次坐标只够一轮用：`ElementHandle` 在**下一次 render 之后会失效**（失效句柄的
    // `absolute_position()`/`size()` 都回 0，拿它算坐标会点到窗口角上），而这里的点击会走
    // **完整的 dispatch + render**（测试里的回调就是真的那一条链）。所以每轮都重新找、重新量。
    let geometry = |window: &AppWindow| {
        let row = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
            window,
            "IdentityRow",
        )
        .next()
        .expect("云存档组里应该有那一条身份");
        let left = row.absolute_position().x;
        let top = row.absolute_position().y;
        let bottom = top + row.size().height;
        // 那条里的按钮（整条里只有一颗，而且靠右）。
        let button =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(window, "StdButton")
                .find(|candidate| {
                    let y = candidate.absolute_position().y;
                    y >= top && y <= bottom && candidate.absolute_position().x > left
                })
                .expect("身份条上应该有那颗按钮");
        (left, (top + bottom) / 2.0, center(&button))
    };

    // ① 点条上的空白：整条可点这条要成立，而且要认住是哪一款。
    let (left, middle, _) = geometry(&window);
    click(&window, LogicalPosition::new(left + 40.0, middle));
    let (open, game_id) = with_ui(|ui| (ui.app.versions.open, ui.app.versions.game_id.clone()));
    assert!(open, "点整条要能进这一款的云端存档页");
    assert_eq!(game_id, "demo");

    // 把那一页收回去，重新量一次（上面那一下点击已经把整页盖住了）。
    with_ui(|ui| {
        ui.app.versions.closed();
        render(ui);
    });

    // ② 点右边那颗按钮：走它自己的动作（打开云端清单），**不许**顺带进版本页。
    let (_, _, button) = geometry(&window);
    click(&window, button);
    let (open, picking) = with_ui(|ui| (ui.app.versions.open, ui.app.cloud_pick.is_open()));
    assert!(
        picking,
        "点「更改绑定…」要真的打开云端清单 —— 事件被整条的 TouchArea 抢走了"
    );
    assert!(!open, "点按钮不该顺带进版本页");
}

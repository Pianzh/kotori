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
        pairing: Rc::new(VecModel::default()),
        cloud_rows: Rc::new(VecModel::default()),
        cloud_versions: Rc::new(VecModel::default()),
        add_match_rows: Rc::new(VecModel::default()),
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

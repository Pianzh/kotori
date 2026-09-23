//! 拖动指针要真的能滚动页面。
//!
//! 这是**没有滚轮的目标**(ARM 手机/平板)能不能用的前提,所以单独量一条。
//!
//! 踩过的坑(2026-09-14,设备上实测):fluent 风格的 `ScrollView` 把内部 `Flickable` 的
//! `interactive` 默认设成 `false`,而**滚轮滚动根本不看这个开关** —— 于是桌面上一切正常,
//! 手机上(只有触摸、没有滚轮)整个应用滚不动:窗口尺寸对、内容确实被截断,就是怎么拖都不动。
//! `ScrollView` 把它暴露成 `mouse-drag-pan-enabled`,我们五个页面都得显式打开。
//!
//! 量两件事:① 内容跟着手指走;② 拖动**不能**被当成一次点击(那会顺带进详细设置)。

use std::rc::Rc;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{LogicalPosition, VecModel};

use super::*;
use crate::ui::test_support::ui_game;

/// 按下、分几步移动、再抬起 —— 和一根手指划过去同一串事件。
fn drag(window: &AppWindow, from: LogicalPosition, dy: f32) {
    let surface = window.window();
    surface.dispatch_event(WindowEvent::PointerMoved { position: from });
    surface.dispatch_event(WindowEvent::PointerPressed {
        position: from,
        button: PointerEventButton::Left,
    });
    const STEPS: u32 = 8;
    for step in 1..=STEPS {
        let moved = LogicalPosition::new(from.x, from.y + dy * step as f32 / STEPS as f32);
        surface.dispatch_event(WindowEvent::PointerMoved { position: moved });
    }
    surface.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(from.x, from.y + dy),
        button: PointerEventButton::Left,
    });
}

/// 第一行的纵坐标 —— 内容滚上去之后它会变小。
fn first_row_top(window: &AppWindow) -> f32 {
    i_slint_backend_testing::ElementHandle::find_by_element_type_name(window, "GameRow")
        .next()
        .expect("游戏库里应该有行")
        .absolute_position()
        .y
}

#[test]
fn dragging_the_pointer_scrolls_the_list() {
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
        process_rows,
        saves_built: Vec::new(),
        saves_seed: 0,
        detail_seed: 0,
    };
    // 塞满一屏还多:内容一定要溢出,否则"滚不动"和"没得滚"分不清。
    ui.app.games = (0..24)
        .map(|index| {
            let mut game = ui_game();
            game.id = format!("demo-{index}");
            game.name = format!("Demo Game {index}");
            game
        })
        .collect();
    render(&mut ui);
    crate::ui::driver::install_state(ui);
    let window = handle.upgrade().expect("窗口还在");
    crate::ui::wire::install_callbacks(&window);

    let before = first_row_top(&window);
    let row = i_slint_backend_testing::ElementHandle::find_by_element_type_name(&window, "GameRow")
        .next()
        .expect("游戏库里应该有行");
    let start = row.absolute_position();
    let size = row.size();

    // 手指往上划 = 看下面的内容 = 内容整体上移。
    drag(
        &window,
        LogicalPosition::new(start.x + size.width / 2.0, start.y + size.height / 2.0),
        -160.0,
    );

    let after = first_row_top(&window);
    assert!(
        after < before - 60.0,
        "拖动指针必须滚动内容(滚轮那条路不看 interactive,所以只有这里能拦住回归):\
         拖动前第一行在 {before},拖动后 {after}"
    );
    assert!(
        !window.get_game_open(),
        "一次拖动是滚动,不是点击 —— 不许顺带下钻到详细设置"
    );
}

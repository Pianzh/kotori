//! Debug-only: photograph the window without looking at the screen.
//!
//! The window is drawn by Slint, so its contents cannot be asserted from a test
//! the way a widget tree could; the only honest way to inspect a layout is to
//! look at pixels. This module makes that possible from a terminal — and only in
//! debug builds, so a release binary carries no back door:
//!
//! - `KOTORI_UI_SNAPSHOT=<path.ppm>` — write the window to that file and quit.
//! - `KOTORI_UI_SNAPSHOT_DELAY=<ms>` — when to take it (default 2500: the first
//!   load talks to the daemon, and a screenshot of "loading" proves nothing).
//! - `KOTORI_UI_TAB=<0..3>`, `KOTORI_UI_SELECT=<game id>`, `KOTORI_UI_SEARCH=<text>`
//!   — driven through the message loop (not by poking properties), so what is
//!   photographed is the real page for that state.
//! - `KOTORI_UI_SEED_DELAY=<ms>` — when to apply the three above (default 1500).
//! - `KOTORI_UI_PICK=<path>` — pretend the file dialog returned that path for the
//!   single game page's 游戏根目录. It is the only way to photograph the
//!   "「浏览…」→ 页面自己填那个框" chain: the test backend's
//!   `accessible_value()` is stale, so a headless assertion would lie (see
//!   `render/window_test.rs`).
//!
//! PPM on purpose: it needs no encoder, and one line of Python turns it into
//! something viewable. (The `png` entry in `Cargo.toml` has no caller — nothing
//! ever asked it for a PNG — so that dependency line is dead weight.)

use std::time::Duration;

use super::*;

/// The chosen snapshot target, or `None` when nobody asked for one.
fn target() -> Option<String> {
    std::env::var("KOTORI_UI_SNAPSHOT")
        .ok()
        .filter(|path| !path.is_empty())
}

fn millis(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

/// Drive the UI into the state the operator asked for.
fn seed() {
    if let Ok(index) = std::env::var("KOTORI_UI_TAB")
        && let Ok(index) = index.parse::<i32>()
    {
        let tab = match index {
            1 => Tab::Add,
            2 => Tab::Cloud,
            3 => Tab::Sync,
            4 => Tab::Settings,
            _ => Tab::Games,
        };
        // `tab` 是窗口自己的属性(点导航栏时由 .slint 直接改),所以这里要两边都做 ——
        // 只发消息的话页面根本不会切过去。
        with_ui(|ui| ui.window.set_tab(index));
        dispatch(Message::TabChanged(tab));
    }
    if let Ok(text) = std::env::var("KOTORI_UI_SEARCH") {
        dispatch(Message::SearchChanged(text));
    }
    if let Ok(id) = std::env::var("KOTORI_UI_SELECT") {
        dispatch(Message::GameSelected(id));
        with_ui(|ui| {
            ui.window.set_game_open(true);
            ui.reseed_detail();
        });
    }
    // 走过整条链:草稿 → 令牌 → 页面自己填那个输入框(只有快照看得见,见文件头)。
    if let Ok(path) = std::env::var("KOTORI_UI_PICK") {
        let _ = with_ui(|ui| {
            ui.app
                .apply_picked_path(PathTarget::GameDir, std::path::Path::new(&path))
        });
    }
}

#[cfg(debug_assertions)]
pub(super) fn install_capture(window: &slint::Weak<AppWindow>) {
    let Some(path) = target() else {
        return;
    };

    let weak = window.clone();
    slint::Timer::single_shot(
        Duration::from_millis(millis("KOTORI_UI_SEED_DELAY", 1500)),
        seed,
    );

    slint::Timer::single_shot(
        Duration::from_millis(millis("KOTORI_UI_SNAPSHOT_DELAY", 2500)),
        move || {
            if let Some(window) = weak.upgrade() {
                match window.window().take_snapshot() {
                    Ok(buffer) => match write_ppm(&path, &buffer) {
                        Ok(()) => println!(
                            "snapshot -> {path} ({}x{})",
                            buffer.width(),
                            buffer.height()
                        ),
                        Err(error) => eprintln!("snapshot 写盘失败: {error}"),
                    },
                    Err(error) => eprintln!("snapshot 失败: {error}"),
                }
            }
            let _ = slint::quit_event_loop();
        },
    );
}

/// A release build has no way to ask for one: `KOTORI_UI_SNAPSHOT` silently
/// does nothing there (by design — this module is debug-only, see the module
/// header), because `install_capture` is only implemented under
/// `#[cfg(debug_assertions)]`.
#[cfg(not(debug_assertions))]
pub(super) fn install_capture(_window: &slint::Weak<AppWindow>) {}

#[cfg(debug_assertions)]
fn write_ppm(
    path: &str,
    buffer: &slint::SharedPixelBuffer<slint::Rgba8Pixel>,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{} {}\n255\n", buffer.width(), buffer.height())?;
    for pixel in buffer.as_slice() {
        out.write_all(&[pixel.r, pixel.g, pixel.b])?;
    }
    out.flush()
}

//! Iced GUI for kotori.
//!
//! This used to be one 4000-line module; it is now split by role: data
//! ([`model`]), messages ([`message`]), the application state ([`app`]) and its
//! message loop ([`update`]), the daemon calls that loop awaits ([`tasks`]), the
//! widget tree ([`view`]), JSON <-> struct conversion ([`parse`]), and the two
//! leaves that touch none of it ([`font`], [`crash`]).
//!
//! Everything the modules share is visible to them through `use super::*;`,
//! which pulls in the imports below plus the `use` globs that follow them.

use std::path::{Path, PathBuf};

use iced::widget::{
    button, container, horizontal_rule, pick_list, row, scrollable, slider, text, text_input,
    toggler,
};
use iced::{Color, Element, Length, Size, Task, Theme};
use serde_json::Value;

use crate::config::{ScaleAlgorithm, ScaleProfile};

mod app;
mod crash;
mod font;
mod message;
mod model;
mod parse;
mod tasks;
mod update;
mod view;

pub use app::App;
pub use message::{Message, SyncField, Tab};
pub use model::{SavePathDraft, SessionInfo, SyncGameRow, SyncStatus, UiGame, WineStatus};

use crash::*;
use font::*;
use model::*;
use parse::*;
use tasks::*;

pub fn run() -> anyhow::Result<()> {
    // Niri + wgpu Vulkan swapchain constantly reports SurfaceError::Outdated,
    // producing an ERROR log storm (33k lines in 6s). Force GL/EGL by default;
    // an explicit WGPU_BACKEND env from the user still takes precedence.
    if std::env::var_os("WGPU_BACKEND").is_none() {
        unsafe { std::env::set_var("WGPU_BACKEND", "gl") };
    }

    let (crash_log, crash_log_ok) = install_crash_log();
    tracing::info!(
        "UI 渲染后端 {}；崩溃报告 {}",
        std::env::var("WGPU_BACKEND").unwrap_or_else(|_| "自动".into()),
        if crash_log_ok {
            crash_log.display().to_string()
        } else {
            format!("写不进 {}（目录不可写）", crash_log.display())
        }
    );

    let socket = crate::config::socket_path();

    // Make sure the daemon is up; if it cannot be started the UI still opens
    // and simply reports「未连接」.
    if let Err(e) = crate::daemon::ensure_running(&socket) {
        tracing::warn!("{e}");
    }

    iced::application("Kotori", App::update, App::view)
        .font(UI_FONT_BYTES)
        .default_font(ui_font())
        .window(iced::window::Settings {
            size: Size::new(960.0, 640.0),
            min_size: Some(Size::new(760.0, 520.0)),
            ..Default::default()
        })
        .theme(|_| Theme::Dark)
        .run_with(App::new)
        .map_err(|e| anyhow::anyhow!("UI error: {e}"))?;

    // Reaching this line means the event loop ended because every window was
    // closed — not because of a panic. Worth recording: "it just exited" is
    // ambiguous otherwise.
    tracing::info!("UI 退出：所有窗口已关闭（不是崩溃）");
    Ok(())
}

#[cfg(test)]
mod test_support {
    use super::*;

    /// Building the widget tree must not panic in any reachable state. This
    /// covers the empty-list / no-search-hit branches of the new pages.
    /// A `sync.status` payload as the daemon sends it.
    pub(super) fn sync_payload() -> Value {
        serde_json::json!({
            "settings": {
                "enabled": true,
                "endpoint": "",
                "bucket": "kotori-saves",
                "prefix": "kotori",
                "encryption": false,
                "keep_versions": 0
            },
            "enabled": true,
            "rclone": "/usr/bin/rclone",
            "keyring": {
                "backend": "Secret Service (libsecret) (/usr/bin/secret-tool)",
                "ephemeral": false,
                "store": { "kind": "system", "backend": "Secret Service (libsecret)" },
                "secrets_file": "/home/user/.config/kotori/secrets.json",
                "min_master_password": 8
            },
            "secrets": ["b2-key-id", "b2-app-key", "sync-password"],
            "ready": true,
            "problem": null,
            "remote": "kotori:kotori-saves/kotori",
            "password_hint": "secret-tool lookup service kotori account sync-password",
            "games": [
                {
                    "id": "demo",
                    "name": "Demo",
                    "locations": 2,
                    "location_problem": null,
                    "last": {
                        "at": "2026-09-11T10:15:00Z",
                        "ok": true,
                        "action": "上传",
                        "detail": "2 个位置已上传"
                    }
                },
                {
                    "id": "other",
                    "name": "Other",
                    "locations": 0,
                    "location_problem": null,
                    "last": null
                }
            ]
        })
    }

    pub(super) fn sync_status_fixture() -> SyncStatus {
        parse_sync_status(&sync_payload()).unwrap()
    }

    pub(super) fn ui_game() -> UiGame {
        UiGame {
            id: "demo".into(),
            name: "Demo Game".into(),
            game_dir: "/games/demo".into(),
            exe: "/games/demo/game.exe".into(),
            save_paths: Vec::new(),
            watch_only: false,
            process_name: String::new(),
            profile_name: "默认".into(),
            algo: "Fsr".into(),
            sharpness: 2,
            internal: (1280, 720),
            output: (2560, 1440),
            scale_ratio: None,
            follow_window: true,
            fullscreen: true,
            framerate: None,
        }
    }
}

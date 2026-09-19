//! Slint GUI for kotori.
//!
//! The front end was rebuilt on Slint in 2026-09-13 (ADR-018: the target is
//! "maximise the Windows 11 resemblance", and iced had no animation system and
//! could not talk to a Wayland input method). What changed is *only* how the
//! window is drawn; the layers underneath are the ones that were already
//! verified and they are untouched:
//!
//! - [`model`] — the plain data, [`message`] — the Elm-style message enum,
//!   [`update`] — the message loop, [`tasks`] — the daemon calls it awaits,
//!   [`parse`] — JSON <-> struct conversion.
//! - [`task`] — the one thing iced owned: `update` still returns
//!   `Task<Message>`, but the type is ours now.
//!
//! What is new is the bridge to the window: [`render`] pushes state into the
//! window's properties, [`wire`] turns its callbacks into messages, and
//! [`driver`] owns the message loop and the tokio runtime behind it. The
//! `.slint` sources live in `src/ui/slint/` and are compiled by `build.rs`.
//!
//! Everything the modules share is visible to them through `use super::*;`,
//! which pulls in the imports below plus the `use` globs that follow them.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{ScaleAlgorithm, ScaleProfile};

slint::include_modules!();

mod app;
mod backend;
mod crash;
mod driver;
mod font;
mod message;
mod model;
mod parse;
mod render;
mod snapshot;
mod task;
mod tasks;
mod update;
mod wire;

pub use app::App;
pub use message::{Message, PathTarget, SyncField, Tab};
pub use model::{SavePathDraft, SessionInfo, SyncGameRow, SyncStatus, UiGame, WineStatus};

/// 界面跑在哪个平台上。
///
/// **编译期就知道,不用问守护进程** —— 界面和守护进程本来就是同一个可执行文件,
/// 永远同平台,为这件事跑一趟 RPC 只会多一个"还没问到"的空窗。
///
/// 用途只有一个:把**只在一边成立**的整块藏掉。Windows 上不做缩放、也不用 wine,
/// 那两块的输入框摆在那儿只会让人以为填错了什么(用户 2026-09-18:「wine 目录多余」、
/// 「windows 的缩放下面带一行字,无效」)。
pub(super) const IS_WINDOWS: bool = cfg!(windows);

use crash::*;
use driver::*;
use model::*;
use parse::*;
use render::*;
use task::*;
use tasks::*;

pub fn run() -> anyhow::Result<()> {
    let (crash_log, crash_log_ok) = install_crash_log();
    tracing::info!(
        "UI 启动；崩溃报告 {}",
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

    // 没有 GPU 的机器上 femtovg 会在建窗口那一刻失败(见 `backend`),这里负责换软件
    // 渲染重开一次,而不是让用户对着 "Could not locate glCreateShader symbol" 发呆。
    backend::finish(driver::run())
}

#[cfg(test)]
mod test_support {
    use super::*;

    /// A `sync.status` payload as the daemon sends it.
    pub(super) fn sync_payload() -> Value {
        serde_json::json!({
            "settings": {
                "enabled": true,
                "endpoint": "",
                "bucket": "kotori-saves",
                "prefix": "kotori",
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
            "secrets": ["b2-key-id", "b2-app-key"],
            "ready": true,
            "problem": null,
            "remote": "kotori:kotori-saves/kotori",
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
            launch_args: Vec::new(),
            save_paths: Vec::new(),
            watch_only: false,
            direct_launch: false,
            process_name: String::new(),
            profile_name: "默认".into(),
            algo: "Fsr".into(),
            sharpness: 2,
            internal: (Some(1280), Some(720)),
            output: (Some(2560), Some(1440)),
            scale_ratio: None,
            fullscreen: true,
            framerate: None,
            gamescope_args: Vec::new(),
        }
    }
}

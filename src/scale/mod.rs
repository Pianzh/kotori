//! The scaling engine: what a game is launched with, how it is scaled while it
//! runs, and what kotori remembers about it.
//!
//! The pieces live apart on purpose — this file used to be all of them at once:
//!
//! * [`action`] — what the CLI and the GUI can ask for, and the ratios involved.
//! * [`args`] — gamescope's command line, whose parameter model is
//!   version-sensitive enough to deserve its own tests.
//! * [`gamescope`] — the engine: sessions, launching, and teardown.
//! * [`teardown`] — the watchdog that puts a ceiling on a wedged shutdown.
//! * [`x11`] — the only module that speaks X11, for gamescope's runtime knobs.
//!
//! The types below are what those modules share; everything else is re-exported
//! so the rest of the crate keeps naming them `scale::…`.

/// 「缩放的阶梯与目标比例」这点算术。
///
/// ⚠ **刻意不在下面那条 cfg 线里**：它只依赖 `config::ScaleProfile`，没有一处碰
/// gamescope / X11 / libc，所以两端都能用（界面读的也是它）。把它一起 cfg 掉会
/// 连带 UI 一起断。
pub mod action;

// Linux 的全部实现:gamescope 的命令行、X11 上那些运行时旋钮、收尾用的进程组
// 看门狗。Windows 上一个都不存在 —— 那边是下面的 `unsupported`。
#[cfg(unix)]
pub mod args;
#[cfg(unix)]
pub mod gamescope;
#[cfg(unix)]
pub mod teardown;
#[cfg(unix)]
pub mod x11;

// Windows 那一侧:一个什么都不做的后端。它为什么是空的（而不是"还没写"）见
// `unsupported` 开头的说明。
#[cfg(windows)]
pub mod unsupported;

use std::path::{Path, PathBuf};

use crate::config::ScaleProfile;

pub use action::ScaleAction;

// 阶梯算术的消费者只有 gamescope 那条路:启动参数、运行时旋钮,以及界面里显示
// 缩放档位的那半。Windows 上缩放后端是空的(`unsupported`),没人读它们 ——
// 但 `action` 模块本身仍在外面,因为它不碰 gamescope / X11 / libc。
#[cfg(unix)]
pub use action::{
    SCALE_LADDER, ladder_index_for, ladder_step, profile_ratio, toggle_target, toggled_ratio,
};

#[cfg(unix)]
pub use args::{build_gamescope_args, sharpness_to_gamescope};

/// 这台机器上真正持有的那个缩放后端。
///
/// 上层（`daemon`）只认这个名字：**后端的差别到这里为止**，再往上就是同一套代码。
/// 从前它直接叫 `GamescopeScaleEngine`，那个名字在 Windows 上会变成一个谎
/// —— 那里根本没有 gamescope。
#[cfg(unix)]
pub use gamescope::GamescopeScaleEngine as PlatformEngine;
#[cfg(windows)]
pub use unsupported::UnsupportedScaleEngine as PlatformEngine;

/// Everything needed to start (or start watching) one game.
#[derive(Debug, Clone)]
pub struct LaunchSpec<'a> {
    /// Id of the game this session belongs to.
    pub game_id: &'a str,
    /// Executable to run (through wine). Unused when `watch_only`.
    pub exe: &'a str,
    /// Extra arguments for that executable.
    pub args: &'a [String],
    /// Working directory — the game root, so the game finds its own assets and
    /// relative save paths line up.
    pub game_dir: &'a Path,
    /// Wine prefix to run under, when one could be resolved.
    pub wine_prefix: Option<&'a Path>,
    /// Scaling to apply to this launch.
    pub profile: &'a ScaleProfile,
    /// Process whose lifetime defines the session. Needed to make a session
    /// outlive a launcher, and required when `watch_only`.
    pub process_name: Option<&'a str>,
    /// Do not launch anything: only track `process_name`. Used for games the
    /// user starts themselves (the norm on Windows).
    pub watch_only: bool,
}

/// ScaleEngine trait - core abstraction for scaling backends
#[async_trait::async_trait]
pub trait ScaleEngine: Send + Sync {
    /// Start a scaling session (gamescope + game)
    async fn start_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError>;

    /// Stop a scaling session
    async fn stop_session(&self, session: &ScaleSession) -> Result<(), ScaleError>;

    /// Wait for a scaling session to exit (i.e. game closed).
    async fn wait_session(&self, session: &ScaleSession) -> Result<(), ScaleError>;

    /// Look up a live session by id. `None` once the game has exited.
    async fn get_session(&self, session_id: &str) -> Option<ScaleSession>;

    /// All live sessions.
    async fn list_sessions(&self) -> Vec<ScaleSession>;

    /// Subscribe to session lifecycle events.
    ///
    /// The daemon uses these to trigger save sync (upload after a game exits,
    /// pull before it starts). Returning `None` is allowed: a backend that
    /// cannot report them simply has no sync triggers, which is a supported
    /// state rather than an error. The session map stays the only source of
    /// truth about *what is running* — these events only say that something
    /// happened.
    fn subscribe(&self) -> Option<tokio::sync::broadcast::Receiver<SessionEvent>> {
        None
    }

    // Runtime scaling control is deliberately *not* part of this trait: it is
    // gamescope's own state, reachable only through the properties gamescope
    // watches on its internal Xwayland ([`x11`]). `GamescopeScaleEngine` implements it
    // as an inherent method, because that is the only backend with a gamescope to
    // talk to — a backend with a real API of its own would grow its own method
    // here.

    /// Get current status
    async fn get_status(&self, session: &ScaleSession) -> Result<ScaleStatus, ScaleError>;
}

/// What the engine noticed about a session's lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    pub session_id: String,
    /// The game this session belongs to, when it is a library entry.
    pub game_id: Option<String>,
    pub kind: SessionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// A session was created (the game is starting, or we started watching).
    Started,
    /// A game actually ran and has now stopped. Never emitted for a watch-only
    /// session whose process never appeared: nothing ran, so there is nothing
    /// worth syncing.
    Ended,
}

/// A live session: either a game kotori launched, or a process it only watches.
#[derive(Debug, Clone)]
pub struct ScaleSession {
    pub session_id: String,
    /// Which game this session belongs to, so clients can match session to game.
    pub game_id: Option<String>,
    /// `None` for watch-only sessions: kotori launched nothing.
    pub gamescope_pid: Option<u32>,
    pub profile: ScaleProfile,
    /// The output size this session's window was opened at, in physical pixels:
    /// what the profile asked for, or the screen (see
    /// [`ScaleProfile::output_size_for`]).
    ///
    /// Recorded at launch rather than re-derived, because it is the one number that
    /// actually describes this session — and the one clients want when they ask what
    /// resolution a running game is being drawn at.
    pub output_size: (u32, u32),
    /// The upscale ratio this session is running at *now*: output pixels ÷ the
    /// game's own resolution. It starts as whatever the profile asked for and is
    /// stepped by `ScaleAction::ScaleUp` / `ScaleDown`.
    ///
    /// Tracked rather than read back: the window belongs to the compositor, and
    /// while its geometry can be queried, doing so needs a D-Bus service of our own
    /// for every keypress. The number only has to be good enough for "one step
    /// further", and a session is rebuilt from its profile every launch.
    pub runtime_ratio: f32,
    pub started_at: std::time::Instant,
    /// Process group to signal on stop; `None` when there is nothing to kill.
    pub process_group: Option<u32>,
    /// The process this session follows, if any.
    pub process_name: Option<String>,
    /// The wine prefix this session's game runs under.
    ///
    /// Kept because part of a real teardown happens *inside* wine: wine's
    /// `winedevice.exe` ignores `SIGTERM`, so it outlives every process-group and
    /// process-tree kill, and only `wineserver -k` on this exact prefix removes it
    /// (see [`crate::wine::close_prefix`]). Left-behind copies are what turn a
    /// logout into a 90 s wait.
    ///
    /// `None` for watch-only sessions: kotori launched nothing, so it has no
    /// business shutting down a prefix the user may be using themselves.
    pub wine_prefix: Option<PathBuf>,
    /// True when kotori did not launch the game, only watched it.
    pub watch_only: bool,
}

/// Current scaling status
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScaleStatus {
    pub fsr_enabled: bool,
    pub current_sharpness: u32,
    pub integer_scaling: bool,
    pub current_resolution: (u32, u32),
}

/// Scaling errors
#[derive(Debug, thiserror::Error)]
pub enum ScaleError {
    #[error("gamescope not found")]
    GamescopeNotFound,

    #[error("gamescope failed to start: {0}")]
    GamescopeStartFailed(String),

    #[error("wine not found")]
    WineNotFound,

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("protocol error: {0}")]
    ProtocolError(String),

    /// 这台机器上没有可用的缩放后端。
    ///
    /// 目前只有 Windows 走到这里：缩放归外部工具（Magpie）管，而它只让观察、
    /// 不让下命令，所以 kotori 没有可做的动作。**它不是"出错了"，而是"这件事
    /// 在这里不归我们"** —— 界面该据此把缩放相关的编辑禁掉，而不是显示成失败。
    ///
    /// ⚠ `allow(dead_code)` 是必要的：Linux 上有真的 gamescope 后端，永远不会构造
    /// 这个变体，而 CI 的 lint 把 warning 当错误（`clippy -- -D warnings`）。它在这里
    /// 不是死代码，是**另一个平台的出口**。
    #[allow(dead_code)]
    #[error("this platform has no scaling backend: scaling belongs to an external tool here")]
    Unsupported,
}

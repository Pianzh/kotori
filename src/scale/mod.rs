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

pub mod action;
pub mod args;
pub mod gamescope;
pub mod teardown;
pub mod x11;

use std::path::{Path, PathBuf};

use crate::config::ScaleProfile;

pub use action::{
    SCALE_LADDER, ScaleAction, ladder_index_for, ladder_step, profile_ratio, toggle_target,
    toggled_ratio,
};
pub use args::{build_gamescope_args, sharpness_to_gamescope};

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
}

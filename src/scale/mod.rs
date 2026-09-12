pub mod niri;
pub mod x11;

use std::path::Path;

use crate::config::{MAX_SHARPNESS, ScaleAlgorithm, ScaleProfile};

/// One runtime scaling action — what a hotkey, the CLI and the GUI all ask for.
///
/// The list is deliberately short: an action exists only if kotori can actually
/// carry it out. gamescope's own `Super+F` (toggle the nested window's
/// fullscreen state) is **not** here, because the runtime channel
/// ([`x11`]) can change the compositor's upscaler and nothing else — and a
/// registered shortcut that cannot do anything is worse than a missing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleAction {
    ToggleFsr,
    ToggleNis,
    ToggleNearest,
    ToggleLinear,
    Soften,
    Sharpen,
}

impl ScaleAction {
    /// Every action kotori registers, in the order the portal lists them.
    pub const ALL: [Self; 6] = [
        Self::ToggleFsr,
        Self::ToggleNis,
        Self::ToggleNearest,
        Self::ToggleLinear,
        Self::Soften,
        Self::Sharpen,
    ];

    /// Stable id: the portal shortcut id, and what the RPC/CLI layer sends.
    pub fn id(self) -> &'static str {
        match self {
            Self::ToggleFsr => "toggle-fsr",
            Self::ToggleNis => "toggle-nis",
            Self::ToggleNearest => "toggle-nearest",
            Self::ToggleLinear => "toggle-linear",
            Self::Soften => "soften",
            Self::Sharpen => "sharpen",
        }
    }

    /// Look an id up as it comes back from the portal.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// The name of this action, as the desktop will show it in its shortcut
    /// list — so it names the *effect*, never a key. The key is the user's to
    /// choose, and gamescope's own chords are not even reachable (see
    /// `crate::hotkeys`).
    ///
    /// The two sharpness steps name the effect, which is the opposite way round
    /// from gamescope's own help text — see [`Self::for_sharpness_delta`].
    pub fn description(self) -> &'static str {
        match self {
            Self::ToggleFsr => "开启/关闭 FSR 放大",
            Self::ToggleNis => "开启/关闭 NIS 放大",
            Self::ToggleNearest => "切换最近邻放大",
            Self::ToggleLinear => "切回双线性过滤",
            Self::Soften => "降低锐度 1 级",
            Self::Sharpen => "提高锐度 1 级",
        }
    }

    /// Trigger suggested to the portal, or `None` for "leave it unbound until
    /// the user asks for it".
    ///
    /// Only the one that matters mid-game comes with a default; everything else
    /// is registered so that it *can* be bound, but bound by choice. Every one
    /// of them is rebindable, from the desktop's shortcut settings today and
    /// from kotori's own settings once it has a page for it.
    ///
    /// A hint is a convenience, never a promise: desktops may ignore it — KDE
    /// does, the string is not even in its portal binary — so the truth is what
    /// the portal reports back (see `crate::hotkeys::HotkeyStatus::unbound`).
    pub fn preferred_trigger(self) -> Option<&'static str> {
        match self {
            // Flipping the upscaling on and off while playing.
            Self::ToggleFsr => Some("<Shift><Alt>q"),
            _ => None,
        }
    }

    /// Which way `delta` steps the sharpness.
    ///
    /// The direction is the opposite of what gamescope's `--help` says, and the
    /// two statements there even contradict each other: `--sharpness` is
    /// documented as "0 (max) to 20 (min)", while `Super+I` is documented as
    /// "increase FSR sharpness by 1" although it does
    /// `g_upscaleFilterSharpness + 1`. The code is the authority — gamescope
    /// hands that number to RCAS as `sharpness / 10`, and FSR's own header says
    /// "0.0 := maximum sharpness, to N>0 … reduction of sharpness"
    /// (`src/shaders/ffx_fsr1.h`). So the *number* is a softness: one step up
    /// softens, one step down sharpens.
    pub fn for_sharpness_delta(delta: i32) -> Option<Self> {
        match delta {
            d if d > 0 => Some(Self::Sharpen),
            d if d < 0 => Some(Self::Soften),
            _ => None,
        }
    }
}

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
    // watches on its internal Xwayland ([`x11`]). `NiriScaleEngine` implements it
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
    pub started_at: std::time::Instant,
    /// Process group to signal on stop; `None` when there is nothing to kill.
    pub process_group: Option<u32>,
    /// The process this session follows, if any.
    pub process_name: Option<String>,
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

/// Translate kotori's internal sharpness (0 = softest, 5 = sharpest) into
/// gamescope's `--sharpness`, whose scale is inverted (0 = max, 20 = min).
pub fn sharpness_to_gamescope(sharpness: u32) -> u32 {
    20 - sharpness.min(MAX_SHARPNESS) * 4
}

/// Build gamescope command line arguments from a scale profile.
///
/// targets gamescope >= 3.16 parameter model:
///   -w/-h :: game (nested) resolution
///   -W/-H :: output resolution
///   -S    :: scaler type (auto, integer, fit, fill, stretch)
///   -F    :: filter (linear, nearest, fsr, nis, pixel)
///   --sharpness :: 0 (max) .. 20 (min)
///
/// `-s` is `--mouse-sensitivity` since 3.16 and must never be emitted here
/// (it swallows the following argument), and `--fsr-sharpness` is only an
/// alias of `--sharpness` that older code passed with the wrong polarity.
///
/// `-W/-H` are the *initial* output size in physical pixels, taken from
/// [`ScaleProfile::output_size`] (scaling ratio first, stored output size as the
/// fallback). gamescope treats them as a preferred size only: a nested window
/// stays freely resizable and gamescope follows every resize by adopting the new
/// content size as its output size, so `follow_window = false` cannot be
/// expressed here — it needs the compositor (see `config::ScaleProfile`).
pub fn build_gamescope_args(profile: &ScaleProfile, game_cmd: &[String]) -> Vec<String> {
    let (output_width, output_height) = profile.output_size();
    let mut args = vec![
        "-w".into(),
        profile.internal_width.to_string(),
        "-h".into(),
        profile.internal_height.to_string(),
        "-W".into(),
        output_width.to_string(),
        "-H".into(),
        output_height.to_string(),
    ];

    // Scale algorithm -> scaler/filter/sharpness.
    match &profile.algorithm {
        ScaleAlgorithm::Fsr { sharpness } => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("fsr".into());
            args.push("--sharpness".into());
            args.push(sharpness_to_gamescope(*sharpness).to_string());
        }
        ScaleAlgorithm::Nis { sharpness } => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("nis".into());
            args.push("--sharpness".into());
            args.push(sharpness_to_gamescope(*sharpness).to_string());
        }
        ScaleAlgorithm::Integer => {
            args.push("-S".into());
            args.push("integer".into());
            args.push("-F".into());
            args.push("nearest".into());
        }
        ScaleAlgorithm::Bilinear => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("linear".into());
        }
    }

    if let Some(fps) = profile.framerate_limit {
        args.push("-r".into());
        args.push(fps.to_string());
    }

    if profile.force_fullscreen {
        args.push("-f".into());
    }

    // Separator
    args.push("--".into());

    // Game command
    args.extend(game_cmd.iter().cloned());

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScaleAlgorithm;

    fn profile(algorithm: ScaleAlgorithm) -> ScaleProfile {
        ScaleProfile {
            algorithm,
            framerate_limit: None,
            force_fullscreen: false,
            ..ScaleProfile::default_for((2560, 1440))
        }
    }

    fn game_cmd() -> Vec<String> {
        vec!["/usr/bin/wine".into(), "/games/x/game.exe".into()]
    }

    /// Index of a flag, or panic with a readable message.
    fn index_of(args: &[String], flag: &str) -> usize {
        args.iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing from {args:?}"))
    }

    #[test]
    fn resolutions_are_mapped_to_geometry_flags() {
        let args = build_gamescope_args(&profile(ScaleAlgorithm::Integer), &game_cmd());
        assert_eq!(
            &args[..8],
            ["-w", "1280", "-h", "720", "-W", "2560", "-H", "1440"]
        );
    }

    #[test]
    fn a_scaling_ratio_overrides_the_stored_output_size() {
        let mut p = profile(ScaleAlgorithm::Fsr { sharpness: 2 });
        p.output_width = 2560;
        p.output_height = 1440;
        p.scale_ratio = Some(1.5);
        let args = build_gamescope_args(&p, &game_cmd());
        // 1280x720 * 1.5, not the stored 2560x1440.
        assert_eq!(
            &args[..8],
            ["-w", "1280", "-h", "720", "-W", "1920", "-H", "1080"]
        );
    }

    #[test]
    fn without_a_ratio_the_stored_output_size_still_drives_the_window() {
        let mut p = profile(ScaleAlgorithm::Integer);
        p.output_width = 1600;
        p.output_height = 900;
        assert_eq!(p.scale_ratio, None);
        let args = build_gamescope_args(&p, &game_cmd());
        assert_eq!(
            &args[..8],
            ["-w", "1280", "-h", "720", "-W", "1600", "-H", "900"]
        );
    }

    #[test]
    fn follow_window_is_not_a_command_line_flag() {
        // gamescope always follows the window; the switch is enforced by the
        // compositor, so it must never leak into the argument list.
        let mut pinned = profile(ScaleAlgorithm::Integer);
        pinned.follow_window = false;
        let floating = profile(ScaleAlgorithm::Integer);
        assert_eq!(
            build_gamescope_args(&pinned, &game_cmd()),
            build_gamescope_args(&floating, &game_cmd())
        );
    }

    #[test]
    fn fsr_uses_the_316_scaler_model() {
        let args =
            build_gamescope_args(&profile(ScaleAlgorithm::Fsr { sharpness: 2 }), &game_cmd());
        assert_eq!(args[index_of(&args, "-S") + 1], "fit");
        assert_eq!(args[index_of(&args, "-F") + 1], "fsr");
        assert_eq!(args[index_of(&args, "--sharpness") + 1], "12");
    }

    #[test]
    fn never_emits_the_legacy_mouse_sensitivity_flag() {
        // `-s` used to mean FSR and now eats the next argument; `--fsr-sharpness`
        // had inverted polarity. Neither may come back.
        for algorithm in [
            ScaleAlgorithm::Fsr { sharpness: 3 },
            ScaleAlgorithm::Nis { sharpness: 3 },
            ScaleAlgorithm::Integer,
            ScaleAlgorithm::Bilinear,
        ] {
            let args = build_gamescope_args(&profile(algorithm), &game_cmd());
            assert!(!args.iter().any(|a| a == "-s"), "legacy -s in {args:?}");
            assert!(
                !args.iter().any(|a| a == "--fsr-sharpness"),
                "legacy --fsr-sharpness in {args:?}"
            );
        }
    }

    #[test]
    fn sharpness_polarity_matches_gamescope() {
        assert_eq!(sharpness_to_gamescope(0), 20);
        assert_eq!(sharpness_to_gamescope(2), 12);
        assert_eq!(sharpness_to_gamescope(5), 0);
        // Out-of-range values are clamped, never negative.
        assert_eq!(sharpness_to_gamescope(99), 0);
    }

    #[test]
    fn nis_and_bilinear_pick_their_filters() {
        let nis = build_gamescope_args(&profile(ScaleAlgorithm::Nis { sharpness: 5 }), &game_cmd());
        assert_eq!(nis[index_of(&nis, "-F") + 1], "nis");
        assert_eq!(nis[index_of(&nis, "--sharpness") + 1], "0");

        let bilinear = build_gamescope_args(&profile(ScaleAlgorithm::Bilinear), &game_cmd());
        assert_eq!(bilinear[index_of(&bilinear, "-F") + 1], "linear");
        assert!(!bilinear.iter().any(|a| a == "--sharpness"));
    }

    #[test]
    fn integer_scaling_uses_nearest_neighbour() {
        let args = build_gamescope_args(&profile(ScaleAlgorithm::Integer), &game_cmd());
        assert_eq!(args[index_of(&args, "-S") + 1], "integer");
        assert_eq!(args[index_of(&args, "-F") + 1], "nearest");
    }

    #[test]
    fn optional_flags_are_only_emitted_when_requested() {
        let bare = build_gamescope_args(&profile(ScaleAlgorithm::Integer), &game_cmd());
        assert!(!bare.iter().any(|a| a == "-r"));
        assert!(!bare.iter().any(|a| a == "-f"));

        let mut p = profile(ScaleAlgorithm::Integer);
        p.framerate_limit = Some(60);
        p.force_fullscreen = true;
        let full = build_gamescope_args(&p, &game_cmd());
        assert_eq!(full[index_of(&full, "-r") + 1], "60");
        assert!(full.iter().any(|a| a == "-f"));
    }

    #[test]
    fn game_command_follows_the_separator_last() {
        let args = build_gamescope_args(&profile(ScaleAlgorithm::Integer), &game_cmd());
        let sep = index_of(&args, "--");
        assert_eq!(&args[sep + 1..], ["/usr/bin/wine", "/games/x/game.exe"]);
        // Nothing after the separator may look like a gamescope flag.
        assert_eq!(args.len(), sep + 3);
    }
}

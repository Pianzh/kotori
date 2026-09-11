pub mod niri;

use std::path::Path;

use crate::config::{MAX_SHARPNESS, ScaleAlgorithm, ScaleProfile};

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

    /// Toggle FSR
    async fn toggle_fsr(&self, session: &ScaleSession) -> Result<(), ScaleError>;

    /// Adjust sharpness
    async fn adjust_sharpness(&self, session: &ScaleSession, delta: i32) -> Result<(), ScaleError>;

    /// Toggle integer scaling
    async fn toggle_integer(&self, session: &ScaleSession) -> Result<(), ScaleError>;

    /// Get current status
    async fn get_status(&self, session: &ScaleSession) -> Result<ScaleStatus, ScaleError>;
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
pub fn build_gamescope_args(profile: &ScaleProfile, game_cmd: &[String]) -> Vec<String> {
    let mut args = vec![
        "-w".into(),
        profile.internal_width.to_string(),
        "-h".into(),
        profile.internal_height.to_string(),
        "-W".into(),
        profile.output_width.to_string(),
        "-H".into(),
        profile.output_height.to_string(),
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

//! The runtime scaling actions and the ratios they move between.
//!
//! Split out of `mod.rs`, which had grown into everything the scaling engine
//! touches at once: this is the part a hotkey, the CLI and the GUI all name.
use crate::config::ScaleProfile;

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
    /// One press: draw the game at the ratio its profile configures; press again
    /// and it is back at its own pixels. This is the user's key — the ladder
    /// below exists for the CLI and for anyone who wants more than two states.
    ToggleScale,
    /// Step the upscale ratio up: gamescope's output — and with it the window —
    /// grows, so the game is drawn larger than its own resolution.
    ScaleUp,
    /// Step the ratio back down.
    ScaleDown,
    /// Back to 1:1: the game at its own resolution, no upscaling at all.
    ResetScale,
    /// Fullscreen, as the compositor understands it.
    ToggleFullscreen,
}

/// The ratios a window-scale hotkey steps through.
///
/// 1.0 is the game at its own resolution, and each step is a quarter until 2×;
/// above that the steps get bigger because the point is "as large as the screen
/// allows", not precision. Sizes are clamped to the screen by the compositor
/// anyway, so the top of the ladder is a ceiling rather than a promise.
pub const SCALE_LADDER: [f32; 7] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The ladder position closest to `ratio`, which is where a session starts (from
/// its profile) and what a hotkey steps away from.
pub fn ladder_index_for(ratio: f32) -> usize {
    SCALE_LADDER
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - ratio)
                .abs()
                .partial_cmp(&(*b - ratio).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// One step along the ladder, staying on it.
pub fn ladder_step(index: usize, up: bool) -> usize {
    let last = SCALE_LADDER.len() - 1;
    if up {
        (index + 1).min(last)
    } else {
        index.saturating_sub(1)
    }
}

/// The upscale ratio a profile launches with: output pixels ÷ the game's own
/// resolution.
///
/// The width decides when the two disagree (only rounding can make that happen),
/// and a profile without an internal size falls back to 1.0 — the game at its own
/// size, which is exactly what the ladder's first step means. This is where a
/// session's ratio starts, so the first hotkey press steps from what the user is
/// looking at rather than from a default.
pub fn profile_ratio(profile: &ScaleProfile) -> f32 {
    if profile.internal_width == 0 {
        return 1.0;
    }
    let (width, _) = profile.output_size();
    let ratio = width as f32 / profile.internal_width as f32;
    if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        1.0
    }
}

/// How close two ratios have to be before they count as the same size.
///
/// The ratio is remembered, not measured: 1.25 stays exactly 1.25 through the
/// round trip, so this only absorbs float noise, not a real difference.
pub const RATIO_EPSILON: f32 = 0.001;

/// The ratio one press of the scaling hotkey scales to — 「设定比例」.
///
/// That is the profile's own ratio: `scale_ratio` when the profile sets one, and
/// otherwise the ratio its `output_*` pair encodes. It is the same number the
/// launch used, so the key always means "the size this game is configured for".
pub fn toggle_target(profile: &ScaleProfile) -> f32 {
    profile_ratio(profile)
}

/// One press of the scaling hotkey: to the configured ratio, or back to 1:1.
///
/// A toggle rather than a ladder, because the user asked for *one* key: press and
/// the game is drawn at the ratio the profile configures, press again and it is
/// back at its own pixels. Being at the target is what tells the two apart —
/// which also covers a game that was launched straight into the target size, so
/// the first press cancels rather than doing nothing visible.
pub fn toggled_ratio(current: f32, target: f32) -> f32 {
    if (current - target).abs() < RATIO_EPSILON {
        1.0
    } else {
        target
    }
}

impl ScaleAction {
    /// Every action kotori registers, in the order the portal lists them.
    pub const ALL: [Self; 11] = [
        Self::ToggleScale,
        Self::ToggleFullscreen,
        Self::ToggleFsr,
        Self::ToggleNis,
        Self::ToggleNearest,
        Self::ToggleLinear,
        Self::Soften,
        Self::Sharpen,
        Self::ScaleUp,
        Self::ScaleDown,
        Self::ResetScale,
    ];

    /// Stable id: the portal shortcut id, and what the RPC/CLI layer sends.
    pub fn id(self) -> &'static str {
        match self {
            Self::ToggleScale => "toggle-scale",
            Self::ScaleUp => "scale-up",
            Self::ScaleDown => "scale-down",
            Self::ResetScale => "reset-scale",
            Self::ToggleFullscreen => "toggle-fullscreen",
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
            Self::ToggleScale => "按设定比例缩放／取消缩放（一键开关）",
            Self::ScaleUp => "放大游戏窗口（提高缩放比例）",
            Self::ScaleDown => "缩小游戏窗口（降低缩放比例）",
            Self::ResetScale => "缩放比例回到 1:1（原始像素）",
            Self::ToggleFullscreen => "切换游戏全屏",
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
    /// Only the two that matter mid-game come with a default; everything else
    /// is registered so that it *can* be bound, but bound by choice. Every one
    /// of them is rebindable, from the desktop's shortcut settings today and
    /// from kotori's own settings once it has a page for it.
    ///
    /// A hint is a convenience, never a promise: desktops may ignore it — KDE
    /// does, the string is not even in its portal binary — so the truth is what
    /// the portal reports back (see `crate::hotkeys::HotkeyStatus::unbound`).
    pub fn preferred_trigger(self) -> Option<&'static str> {
        match self {
            // The two things a player reaches for without leaving the game: one
            // key that turns the configured scaling on and off, one that decides
            // whether the game owns the screen. The user asked for exactly these
            // two chords (2026-09-12); everything else is bound by choice.
            Self::ToggleScale => Some("<Shift><Alt>q"),
            Self::ToggleFullscreen => Some("<Shift><Alt>a"),
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

    /// Does this action change gamescope's filter rather than its window?
    ///
    /// The split matters because the two live in different places: filters are
    /// root-window properties on gamescope's own Xwayland ([`x11`]), while the
    /// window size is the compositor's ([`crate::desktop::kde`]).
    pub fn is_filter(self) -> bool {
        matches!(
            self,
            Self::ToggleFsr
                | Self::ToggleNis
                | Self::ToggleNearest
                | Self::ToggleLinear
                | Self::Soften
                | Self::Sharpen
        )
    }
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

    #[test]
    fn the_ladder_starts_at_one_and_stops_at_its_ends() {
        assert_eq!(SCALE_LADDER[0], 1.0);
        assert_eq!(ladder_step(0, true), 1);
        assert_eq!(SCALE_LADDER[ladder_step(0, true)], 1.25);
        // Down from the bottom, and up from the top, stay on the ladder.
        assert_eq!(ladder_step(0, false), 0);
        let top = SCALE_LADDER.len() - 1;
        assert_eq!(ladder_step(top, true), top);
        assert_eq!(ladder_step(top, false), top - 1);
    }

    #[test]
    fn a_profile_ratio_lands_on_its_ladder_step() {
        // The user's own profile: 1280x720 upscaled to a 2560x1440 output.
        assert_eq!(ladder_index_for(2.0), 4);
        assert_eq!(SCALE_LADDER[ladder_index_for(2.0)], 2.0);
        // A ratio between steps picks the nearer one, and a silly one clamps.
        assert_eq!(ladder_index_for(1.3), 1);
        assert_eq!(ladder_index_for(0.0), 0);
        assert_eq!(ladder_index_for(99.0), SCALE_LADDER.len() - 1);
        // One press of "smaller" from the launch ratio is the next step down.
        let step = ladder_step(ladder_index_for(2.0), false);
        assert_eq!(SCALE_LADDER[step], 1.75);
    }

    #[test]
    fn profile_ratio_is_the_one_the_launch_arguments_encode() {
        let profile = profile(ScaleAlgorithm::Fsr { sharpness: 2 });
        // `profile()` is 1280x720 into the display resolution; whatever that is,
        // the ratio is what the output size says it is.
        let (width, _) = profile.output_size();
        assert_eq!(profile_ratio(&profile), width as f32 / 1280.0);

        let mut unscaled = profile;
        unscaled.scale_ratio = Some(1.25);
        assert_eq!(profile_ratio(&unscaled), 1.25);
        assert_eq!(ladder_index_for(profile_ratio(&unscaled)), 1);

        let mut broken = unscaled;
        broken.internal_width = 0;
        assert_eq!(profile_ratio(&broken), 1.0);
    }

    #[test]
    fn one_key_flips_between_the_configured_ratio_and_one_to_one() {
        let mut configured = profile(ScaleAlgorithm::Fsr { sharpness: 2 });
        configured.internal_width = 1280;
        configured.internal_height = 720;
        configured.scale_ratio = Some(1.25);

        // The target is what the profile configures, not the ladder's next step.
        assert_eq!(toggle_target(&configured), 1.25);

        // Launched straight into the configured size: the first press cancels,
        // which is the only reading of "press again to turn it off" that works
        // when there is nothing to grow into.
        assert_eq!(toggled_ratio(1.25, 1.25), 1.0);
        // And from there it comes back.
        assert_eq!(toggled_ratio(1.0, 1.25), 1.25);
        // Pressing from a size nobody asked for (a ladder step, a dragged
        // window) still lands on the configured ratio rather than toggling off.
        assert_eq!(toggled_ratio(1.75, 1.25), 1.25);
        // A profile without an explicit ratio uses the one its output size
        // encodes, so the same key works for games configured the old way.
        let legacy = profile(ScaleAlgorithm::Integer);
        let expected = profile_ratio(&legacy);
        assert_eq!(toggle_target(&legacy), expected);
        assert_eq!(toggled_ratio(expected, expected), 1.0);
        assert_eq!(toggled_ratio(1.0, expected), expected);
    }
}

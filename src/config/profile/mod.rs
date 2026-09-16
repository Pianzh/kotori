//! The scaling profile: what a game is drawn at, and how it is upscaled.
//!
//! Split out of `mod.rs` because it is the half of the config that changes when
//! scaling does, and because the numbers here (resolutions, ratios, sharpness)
//! are a contract with gamescope's command line.

use serde::{Deserialize, Serialize};

/// The size gamescope renders at when nobody passes `-w/-h`.
///
/// Only an *arithmetic* fallback now: a profile that names no game resolution
/// launches without those flags and gamescope picks these numbers itself, so any
/// ratio has to be measured against them. Not a value kotori writes into a
/// game's config — see [`ScaleProfile::internal_width`].
pub const GAMESCOPE_DEFAULT_WIDTH: u32 = 1280;
pub const GAMESCOPE_DEFAULT_HEIGHT: u32 = 720;

/// Last-resort output resolution when no display can be queried.
pub const FALLBACK_OUTPUT_WIDTH: u32 = 1920;
pub const FALLBACK_OUTPUT_HEIGHT: u32 = 1080;

/// Accepted range for any resolution field coming from a client.
pub const MAX_RESOLUTION: u32 = 16384;

/// Accepted range for the scaling ratio (output ÷ internal resolution).
/// 1.0 means "no upscaling"; the product is additionally capped by
/// `MAX_RESOLUTION`, so the ceiling here only catches nonsense.
pub const MIN_SCALE_RATIO: f32 = 0.25;
pub const MAX_SCALE_RATIO: f32 = 8.0;
/// Accepted range for the frame rate limit.
pub const MAX_FRAMERATE: u32 = 1000;

/// Internal sharpness range (0 = softest, 5 = sharpest). Mirrors the UI slider.
pub const MAX_SHARPNESS: u32 = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScaleProfile {
    pub name: String,
    pub algorithm: ScaleAlgorithm,
    /// The resolution the game itself renders at, passed as gamescope's `-w/-h`.
    ///
    /// **Empty by default** (user's call, 2026-09-13): nothing ever probed this
    /// value, so the 1280x720 that used to sit here was a guess written into 42
    /// profiles and never once true. Empty means kotori passes no `-w/-h` at all
    /// and gamescope uses its own default — the same 1280x720, minus the pretence
    /// that someone chose it. Both halves must be present to count, exactly like
    /// [`ScaleProfile::explicit_output_size`].
    #[serde(default)]
    pub internal_width: Option<u32>,
    #[serde(default)]
    pub internal_height: Option<u32>,
    /// An explicit window size, in physical pixels — the override for the automatic
    /// "open at the screen's size".
    ///
    /// **Empty by default** (user's call, 2026-09-13): both fields moved into the
    /// UI's advanced section, and a profile that names neither a ratio nor a size
    /// simply gets the screen. `Option` rather than `0`-means-unset, because "the
    /// user did not say" and "1920x1080" are different answers and a sentinel would
    /// have to be special-cased everywhere they are read.
    #[serde(default)]
    pub output_width: Option<u32>,
    #[serde(default)]
    pub output_height: Option<u32>,
    /// Upscale factor relative to the internal resolution. When present it
    /// *wins* over `output_*` — those stay for profiles written before ratios
    /// existed, and for the UI, which still edits them.
    #[serde(default)]
    pub scale_ratio: Option<f32>,
    #[serde(default)]
    pub framerate_limit: Option<u32>,
    /// `-f`: pin the nested window to the whole output.
    ///
    /// **Off by default** (user's call, 2026-09-13). `-f` makes `g_nOutput` the
    /// screen geometry, so it throws away both the configured ratio *and* the
    /// user's own resizing: KWin cannot shrink a window gamescope has pinned, and
    /// that is what "the window can only get smaller in one direction" turned out
    /// to be. What someone asking for "maximised" actually wants is an ordinary
    /// resizable window that happens to open at the screen's size — which is what
    /// `-W/-H` already give, with `-f` nowhere in sight.
    #[serde(default)]
    pub force_fullscreen: bool,
    /// gamescope 参数,由用户**手写**。
    ///
    /// **非空时它取代上面全部**(用户 2026-09-16 定的语义):kotori 一个参数都不发
    /// —— `-w/-h/-W/-H/-S/-F/--sharpness/-r/-f` 通通让位 —— 只把自己追加的 `--`
    /// 和游戏命令接在这一串后面。空 = 照常按上面的字段拼。
    ///
    /// 切分是**按空白**(见 [`crate::scale::args::build_gamescope_args`]):这是高级
    /// 选项,写法由用户自己负责,kotori 不去猜引号语义(猜错比不猜更难查)。
    ///
    /// 这一模式下**运行时缩放被禁用**(见 [`ScaleProfile::free_form`] 的说明)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gamescope_args: Vec<String>,
}

/// Scaling algorithm bound to a game.
///
/// NOTE: gamescope (>= 3.16) only provides `linear`, `nearest`, `fsr`, `nis`
/// and `pixel` filters — there is no Lanczos filter, so no such variant is
/// offered here (a variant that silently degrades to bilinear is a lie).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleAlgorithm {
    Fsr { sharpness: u32 },
    Nis { sharpness: u32 },
    Integer,
    Bilinear,
}

impl ScaleAlgorithm {
    /// Names shown in the UI / accepted by [`ScaleAlgorithm::from_label`].
    pub const ALL: [&'static str; 4] = ["Fsr", "Nis", "Integer", "Bilinear"];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Fsr { .. } => "Fsr",
            Self::Nis { .. } => "Nis",
            Self::Integer => "Integer",
            Self::Bilinear => "Bilinear",
        }
    }

    /// Sharpness for the algorithms that support it (clamped to 0..=MAX_SHARPNESS).
    pub fn sharpness(&self) -> Option<u32> {
        match self {
            Self::Fsr { sharpness } | Self::Nis { sharpness } => {
                Some((*sharpness).min(MAX_SHARPNESS))
            }
            Self::Integer | Self::Bilinear => None,
        }
    }

    /// Rebuild this algorithm with a new sharpness value (no-op for the
    /// algorithms that ignore sharpness).
    pub fn with_sharpness(self, sharpness: u32) -> Self {
        let sharpness = sharpness.min(MAX_SHARPNESS);
        match self {
            Self::Fsr { .. } => Self::Fsr { sharpness },
            Self::Nis { .. } => Self::Nis { sharpness },
            other => other,
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "Fsr" => Some(Self::Fsr { sharpness: 2 }),
            "Nis" => Some(Self::Nis { sharpness: 2 }),
            "Integer" => Some(Self::Integer),
            "Bilinear" => Some(Self::Bilinear),
            _ => None,
        }
    }
}

impl ScaleProfile {
    /// Sensible default profile for a new game.
    ///
    /// Takes no screen, because kotori no longer writes one machine's resolution
    /// into a game's config: the window size is worked out at launch from the output
    /// the game actually lands on (see [`ScaleProfile::output_size_for`]), so moving
    /// the config to another monitor — or another machine — needs no re-scan.
    pub fn default_for() -> Self {
        Self {
            name: "默认".to_string(),
            algorithm: ScaleAlgorithm::Fsr { sharpness: 2 },
            internal_width: None,
            internal_height: None,
            output_width: None,
            output_height: None,
            scale_ratio: None,
            framerate_limit: None,
            force_fullscreen: false,
            gamescope_args: Vec::new(),
        }
    }

    /// gamescope 的命令行是不是**完全**由用户手写([`ScaleProfile::gamescope_args`])。
    ///
    /// 它不只是"参数怎么拼"的开关,也是**运行时缩放的开关**:那套动作(`ScaleAction`)
    /// 往 gamescope 的 Xwayland 根窗口写滤镜/缩放属性,会当场盖掉用户亲手写的
    /// `-F`/`-S` —— 用户既然说了"我说了算",kotori 就不该在游戏跑起来之后再去动它。
    /// 所以 `apply_action` 会跳过这类会话,`live_settings` 也不回报状态(档案里那个
    /// 算法根本没发给 gamescope,拿它冒充"现在"是撒谎)。
    pub fn free_form(&self) -> bool {
        !self.gamescope_args.is_empty()
    }

    /// The game resolution to *launch* with, when the profile names one.
    ///
    /// Both halves or nothing — half a resolution is not a resolution (the same
    /// rule [`ScaleProfile::explicit_output_size`] follows). `None` means kotori
    /// emits no `-w/-h` and gamescope keeps its own default.
    pub fn explicit_internal_size(&self) -> Option<(u32, u32)> {
        match (self.internal_width, self.internal_height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
            _ => None,
        }
    }

    /// The game's render size for arithmetic: what the profile says, otherwise
    /// gamescope's own default (which is what a launch without `-w/-h` gets).
    ///
    /// Never zero, so callers can divide by it without a guard.
    pub fn internal_size(&self) -> (u32, u32) {
        self.explicit_internal_size()
            .unwrap_or((GAMESCOPE_DEFAULT_WIDTH, GAMESCOPE_DEFAULT_HEIGHT))
    }

    /// The window size this profile *asks for*, if the user filled either field in.
    ///
    /// `scale_ratio` wins when it is usable — it is the more specific statement.
    /// Otherwise the stored pair does, but only when **both** halves are there: half
    /// a size is not a size, and inventing the other half would be worse than
    /// ignoring what was typed.
    ///
    /// `None` means "work it out", which [`ScaleProfile::output_size_for`] does from
    /// the screen. Both are physical pixels, which is what gamescope's `-W/-H`
    /// expect; gamescope divides by the compositor's fractional scale itself, so no
    /// desktop-scale maths belongs here.
    pub fn explicit_output_size(&self) -> Option<(u32, u32)> {
        if let Some(ratio) = self.scale_ratio.filter(|r| r.is_finite() && *r > 0.0) {
            let (internal_width, internal_height) = self.internal_size();
            let upscale = |value: u32| -> u32 {
                ((value as f32 * ratio).round() as i64).clamp(1, MAX_RESOLUTION as i64) as u32
            };
            return Some((upscale(internal_width), upscale(internal_height)));
        }

        match (self.output_width, self.output_height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
            _ => None,
        }
    }

    /// Where the nested window opens: what the profile asked for, or the screen.
    ///
    /// Opening at the screen's size **is** what "maximised" means here, and it is all
    /// `-W/-H` do: they are the *initial* size, and the compositor may resize from
    /// there. Nothing pins the window — that would be `-f`, i.e. `force_fullscreen`.
    pub fn output_size_for(&self, screen: (u32, u32)) -> (u32, u32) {
        self.explicit_output_size().unwrap_or(screen)
    }

    /// Clamp values that are representable but outside the supported range.
    pub fn normalize(&mut self) {
        match self.algorithm {
            ScaleAlgorithm::Fsr { sharpness } => {
                self.algorithm = ScaleAlgorithm::Fsr {
                    sharpness: sharpness.min(MAX_SHARPNESS),
                };
            }
            ScaleAlgorithm::Nis { sharpness } => {
                self.algorithm = ScaleAlgorithm::Nis {
                    sharpness: sharpness.min(MAX_SHARPNESS),
                };
            }
            ScaleAlgorithm::Integer | ScaleAlgorithm::Bilinear => {}
        }
    }

    /// Validate a profile that arrived from a client before it is persisted.
    /// Returns a message that is safe to show to the user.
    pub fn validate(&self) -> Result<(), String> {
        // 留空＝交给 gamescope 自己定,是正常状态;填了才要求合法。两种分辨率
        // (游戏自己的 / 输出的)规则一样,只是名字不同。
        for (label, value) in [
            ("游戏分辨率宽", self.internal_width),
            ("游戏分辨率高", self.internal_height),
            ("输出分辨率宽", self.output_width),
            ("输出分辨率高", self.output_height),
        ] {
            if let Some(value) = value
                && (value == 0 || value > MAX_RESOLUTION)
            {
                return Err(format!(
                    "{label} 必须在 1..={MAX_RESOLUTION} 之间（当前 {value}）"
                ));
            }
        }

        if let Some(fps) = self.framerate_limit
            && (fps == 0 || fps > MAX_FRAMERATE)
        {
            return Err(format!(
                "帧率限制必须在 1..={MAX_FRAMERATE} 之间（当前 {fps}）"
            ));
        }

        if let Some(ratio) = self.scale_ratio {
            if !ratio.is_finite() || !(MIN_SCALE_RATIO..=MAX_SCALE_RATIO).contains(&ratio) {
                return Err(format!(
                    "缩放比例必须在 {MIN_SCALE_RATIO}..={MAX_SCALE_RATIO} 之间（当前 {ratio}）"
                ));
            }
            // Checked before `output_size()` clamps, so a ratio that only looks
            // fine because of the clamp is still rejected here.
            let (internal_width, internal_height) = self.internal_size();
            let wide = internal_width as f64 * ratio as f64;
            let high = internal_height as f64 * ratio as f64;
            if wide > MAX_RESOLUTION as f64 || high > MAX_RESOLUTION as f64 {
                return Err(format!(
                    "缩放比例 {ratio} 会把输出分辨率变成 {:.0}x{:.0}，超过上限 {MAX_RESOLUTION}",
                    wide.round(),
                    high.round()
                ));
            }
        }

        // 自由参数**不做白名单**(它的卖点就是自由,而且 gamescope 各版本的参数还
        // 在变),只挡一样东西:用户自己写 `--`。kotori 要在游戏命令前放一个,用户
        // 再写一个,后面那半截就成了 gamescope 眼里的"命令" —— 报出来的错跟真正
        // 的原因毫无关系。
        //
        // 比的是整段而不是 `contains("--")`:`--sharpness` 这种合法参数里也有两个
        // 连字符,那样写会把它们一起挡掉。
        if self.gamescope_args.iter().any(|arg| arg == "--") {
            return Err("gamescope 参数里不要写 `--`：kotori 自己会在游戏命令前放一个".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;

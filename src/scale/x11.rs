//! Runtime scaling for a *running* gamescope, through its internal Xwayland.
//!
//! gamescope exposes no protocol for this. `gamescope_control` (v7) only has
//! screenshot / refresh rate / LUT / keyboard layout, and `gamescope_private.execute`
//! only runs a handful of debug commands and convars -- none of which touch the
//! upscaler. The two ways in are:
//!
//!  1. gamescope's own hotkeys (`Super+U` …). In the nested (Wayland) backend those
//!     never match: `CWaylandInputThread::HandleKey` compares the raw **Wayland**
//!     keycode against `KEY_*` from `linux/input-event-codes.h`, which are evdev
//!     codes, i.e. off by the 8 the Wayland protocol adds. Reaching them also needs
//!     a way to synthesise keys (portal RemoteDesktop), which drags in keyboard
//!     focus and desktop-specific behaviour.
//!  2. the properties gamescope watches on the root window of its internal
//!     Xwayland (`steamcompmgr.cpp`): `GAMESCOPE_NEW_SCALING_FILTER`,
//!     `GAMESCOPE_NEW_SCALING_SCALER`, `GAMESCOPE_FSR_SHARPNESS`. No permission, no
//!     focus, no compositor involvement -- and it works for X11 *and* Wayland games
//!     alike, because it changes what the compositor does, not what the game does.
//!
//! This module is (2). It is deliberately the only place that speaks X11.
//!
//! Two details worth keeping in mind:
//!
//! * The old and new filter properties disagree on numbering.
//!   `GAMESCOPE_SCALING_FILTER` is `0=linear 1=nearest 2=integer 3=fsr 4=nis`,
//!   while `GAMESCOPE_NEW_SCALING_FILTER` takes the `GamescopeUpscaleFilter` enum
//!   itself (`LINEAR=0 NEAREST=1 FSR=2 NIS=3 PIXEL=4`). We use the new one: it maps
//!   1:1 onto the source enum, so there is nothing to mistranslate.
//! * gamescope never writes these properties back, so they are *our* record of what
//!   we last asked for -- which is what makes a "toggle" possible without a query
//!   API. [`GamescopeDisplay::read`] reads back what we wrote.

use std::path::Path;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode, Window};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::ScaleAlgorithm;

/// Where X sockets live. Only overridable so tests can point at a temp dir.
pub const SOCKET_DIR: &str = "/tmp/.X11-unix";

/// `GamescopeUpscaleFilter` (`src/main.hpp` upstream), in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Filter {
    Linear = 0,
    Nearest = 1,
    Fsr = 2,
    Nis = 3,
    /// gamescope's "pixel" filter -- what its own `Super+N` selects.
    Pixel = 4,
}

/// `GamescopeUpscaleScaler` (`src/main.hpp` upstream), in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Scaler {
    Auto = 0,
    Integer = 1,
    Fit = 2,
    Fill = 3,
    Stretch = 4,
}

/// What the compositor should do with the game's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub filter: Filter,
    pub scaler: Scaler,
    /// gamescope's own sharpness scale: 0 = sharpest, 20 = softest.
    pub sharpness: u32,
}

impl Settings {
    /// The settings a [`ScaleAlgorithm`] corresponds to.
    ///
    /// This must agree with [`super::build_gamescope_args`]: one is what we pass at
    /// launch, the other what we push at runtime, and a user pressing the hotkey must
    /// not land somewhere the launcher would never put them.
    pub fn for_algorithm(algorithm: &ScaleAlgorithm) -> Self {
        match algorithm {
            ScaleAlgorithm::Fsr { sharpness } => Self {
                filter: Filter::Fsr,
                scaler: Scaler::Fit,
                sharpness: super::sharpness_to_gamescope(*sharpness),
            },
            ScaleAlgorithm::Nis { sharpness } => Self {
                filter: Filter::Nis,
                scaler: Scaler::Fit,
                sharpness: super::sharpness_to_gamescope(*sharpness),
            },
            ScaleAlgorithm::Integer => Self {
                filter: Filter::Nearest,
                scaler: Scaler::Integer,
                sharpness: super::sharpness_to_gamescope(0),
            },
            ScaleAlgorithm::Bilinear => Self {
                filter: Filter::Linear,
                scaler: Scaler::Fit,
                sharpness: super::sharpness_to_gamescope(0),
            },
        }
    }

    /// FSR on, or off if it is already on.
    ///
    /// Turning FSR *off* falls back to linear, which is what gamescope's own
    /// `Super+U` does -- "off" should mean "stop sharpening", not "pick another
    /// filter for me".
    pub fn toggled_fsr(self) -> Self {
        self.with_filter(if self.filter == Filter::Fsr {
            Filter::Linear
        } else {
            Filter::Fsr
        })
    }

    /// NIS on, or off if it is already on.
    pub fn toggled_nis(self) -> Self {
        self.with_filter(if self.filter == Filter::Nis {
            Filter::Linear
        } else {
            Filter::Nis
        })
    }

    /// Nearest-neighbour at an integer scale factor.
    pub fn nearest(self) -> Self {
        Self {
            filter: Filter::Nearest,
            scaler: Scaler::Integer,
            ..self
        }
    }

    /// Plain bilinear stretch.
    pub fn bilinear(self) -> Self {
        Self {
            filter: Filter::Linear,
            scaler: Scaler::Fit,
            ..self
        }
    }

    /// One step softer, clamped at gamescope's maximum softness.
    pub fn softer(self) -> Self {
        Self {
            sharpness: (self.sharpness + 1).min(GAMESCOPE_MAX_SHARPNESS),
            ..self
        }
    }

    /// One step sharper, clamped at gamescope's maximum sharpness.
    pub fn sharper(self) -> Self {
        Self {
            sharpness: self.sharpness.saturating_sub(1),
            ..self
        }
    }

    fn with_filter(self, filter: Filter) -> Self {
        Self {
            filter,
            scaler: Scaler::Fit,
            ..self
        }
    }

    /// What an action does to the current setting.
    ///
    /// Toggles go through here rather than through a stored "is FSR on" flag: the
    /// properties on gamescope's root window are the record of what we last asked
    /// for ([`GamescopeDisplay::read`]), so there is only one copy of the truth and
    /// no state to lose when a session is restarted.
    pub fn applied(self, action: super::ScaleAction) -> Self {
        use super::ScaleAction as A;
        match action {
            A::ToggleFsr => self.toggled_fsr(),
            A::ToggleNis => self.toggled_nis(),
            A::ToggleNearest => self.nearest(),
            A::ToggleLinear => self.bilinear(),
            A::Soften => self.softer(),
            A::Sharpen => self.sharper(),
            // The window actions never reach here: the engine routes by
            // `is_filter()` first, and a filter setting has nothing to say about
            // them. Listed explicitly so a new action cannot arrive unnoticed.
            A::ScaleUp | A::ScaleDown | A::ResetScale | A::ToggleFullscreen => self,
        }
    }
}

/// gamescope clamps sharpness to 0..20 (`steamcompmgr.cpp`).
pub const GAMESCOPE_MAX_SHARPNESS: u32 = 20;

#[derive(Debug, thiserror::Error)]
pub enum X11Error {
    #[error("连不上 gamescope 的内部 X 服务器 {display}：{source}")]
    Connect {
        display: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("X 服务器没有 screen")]
    NoScreen,
    #[error("gamescope 报了未知的缩放滤镜值 {0}（预期 0..=4）")]
    UnknownFilter(u32),
    #[error("X 请求失败：{0}")]
    Reply(#[from] x11rb::errors::ReplyError),
    #[error("X 连接断开：{0}")]
    Connection(#[from] x11rb::errors::ConnectionError),
}

x11rb::atom_manager! {
    /// The root-window properties gamescope watches, plus its pid for discovery.
    pub GamescopeAtoms: GamescopeAtomsCookie {
        GAMESCOPE_NEW_SCALING_FILTER,
        GAMESCOPE_NEW_SCALING_SCALER,
        GAMESCOPE_FSR_SHARPNESS,
        GAMESCOPE_PID,
    }
}

/// A live connection to one gamescope's internal Xwayland.
#[derive(Debug)]
pub struct GamescopeDisplay {
    /// `:0`, `:1`, … -- for logs and error messages.
    display: String,
    conn: RustConnection,
    root: Window,
    atoms: GamescopeAtoms,
}

impl GamescopeDisplay {
    /// Find the gamescope whose pid is `pid`.
    ///
    /// gamescope writes its own pid into `GAMESCOPE_PID` on the root window of the
    /// Xwayland it owns (`steamcompmgr.cpp`), so this is an exact match rather than a
    /// guess -- which matters as soon as a user has two games open at once.
    pub fn discover(pid: u32) -> Result<Option<Self>, X11Error> {
        Self::discover_in(Path::new(SOCKET_DIR), pid)
    }

    /// [`discover`](Self::discover) against an arbitrary socket directory.
    ///
    /// Sockets that exist but do not belong to `pid` are skipped silently: a stale
    /// socket from a crashed server, or a second gamescope for the other monitor, is
    /// not an error for *this* lookup. If none of them could even be connected to,
    /// that is reported, because "not running" and "cannot reach it" are different
    /// things to a user.
    pub fn discover_in(dir: &Path, pid: u32) -> Result<Option<Self>, X11Error> {
        let mut last_error = None;
        for number in display_numbers(dir) {
            match Self::open_at_number(number) {
                Ok(display) => match display.gamescope_pid() {
                    Ok(found) if found == pid => return Ok(Some(display)),
                    Ok(_) => {}
                    Err(err) => last_error = Some(err),
                },
                Err(err) => last_error = Some(err),
            }
        }
        match last_error {
            Some(err) => Err(err),
            None => Ok(None),
        }
    }

    /// Connect to display `:<number>`.
    ///
    /// `RustConnection::connect` resolves the display name the same way any X client
    /// does (abstract sockets included) and looks the cookie up in `$XAUTHORITY` /
    /// `~/.Xauthority`. gamescope never sets `XAUTHORITY` for the games it launches
    /// and never mentions xauth in its source, so its Xwayland is normally reachable
    /// without a cookie -- but when one is needed, this handles it.
    pub fn open_at_number(number: u32) -> Result<Self, X11Error> {
        let display = format!(":{number}");
        let (conn, screen) =
            RustConnection::connect(Some(&display)).map_err(|err| X11Error::Connect {
                display: display.clone(),
                source: Box::new(err),
            })?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .map(|screen| screen.root)
            .ok_or(X11Error::NoScreen)?;
        let atoms = GamescopeAtoms::new(&conn)?.reply()?;
        Ok(Self {
            display,
            conn,
            root,
            atoms,
        })
    }

    /// The `:N` this connection is on.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// gamescope's pid, as published by gamescope itself.
    pub fn gamescope_pid(&self) -> Result<u32, X11Error> {
        Ok(self.property(self.atoms.GAMESCOPE_PID)?.unwrap_or_default())
    }

    /// What we last asked for, if we ever asked.
    pub fn read(&self) -> Result<Option<Settings>, X11Error> {
        let Some(filter) = self.property(self.atoms.GAMESCOPE_NEW_SCALING_FILTER)? else {
            return Ok(None);
        };
        let scaler = self
            .property(self.atoms.GAMESCOPE_NEW_SCALING_SCALER)?
            .unwrap_or(Scaler::Auto as u32);
        let sharpness = self
            .property(self.atoms.GAMESCOPE_FSR_SHARPNESS)?
            .unwrap_or_else(|| super::sharpness_to_gamescope(2));
        Ok(Some(Settings {
            filter: filter_from_u32(filter)?,
            scaler: scaler_from_u32(scaler),
            sharpness: sharpness.min(GAMESCOPE_MAX_SHARPNESS),
        }))
    }

    /// Push a whole setting in one go.
    ///
    /// Order matters only for the repaint: every write that changes something makes
    /// gamescope repaint, so the filter goes last and no intermediate frame shows a
    /// scaler/sharpness combination the caller never asked for.
    pub fn apply(&self, settings: Settings) -> Result<(), X11Error> {
        self.set(
            self.atoms.GAMESCOPE_NEW_SCALING_SCALER,
            settings.scaler as u32,
        )?;
        self.set(
            self.atoms.GAMESCOPE_FSR_SHARPNESS,
            settings.sharpness.min(GAMESCOPE_MAX_SHARPNESS),
        )?;
        self.set(
            self.atoms.GAMESCOPE_NEW_SCALING_FILTER,
            settings.filter as u32,
        )?;
        Ok(())
    }

    fn property(&self, atom: x11rb::protocol::xproto::Atom) -> Result<Option<u32>, X11Error> {
        let reply = self
            .conn
            .get_property(false, self.root, atom, AtomEnum::CARDINAL, 0, 1)?
            .reply()?;
        Ok(reply.value32().and_then(|mut values| values.next()))
    }

    fn set(&self, atom: x11rb::protocol::xproto::Atom, value: u32) -> Result<(), X11Error> {
        self.conn
            .change_property32(
                PropMode::REPLACE,
                self.root,
                atom,
                AtomEnum::CARDINAL,
                &[value],
            )?
            .check()?;
        Ok(())
    }
}

fn filter_from_u32(value: u32) -> Result<Filter, X11Error> {
    match value {
        0 => Ok(Filter::Linear),
        1 => Ok(Filter::Nearest),
        2 => Ok(Filter::Fsr),
        3 => Ok(Filter::Nis),
        4 => Ok(Filter::Pixel),
        other => Err(X11Error::UnknownFilter(other)),
    }
}

/// Gamescope's enum is exactly 0..=4, so anything else is a value another writer put
/// there. Falling back to `AUTO` keeps a stale property from making the display
/// unusable.
fn scaler_from_u32(value: u32) -> Scaler {
    match value {
        1 => Scaler::Integer,
        2 => Scaler::Fit,
        3 => Scaler::Fill,
        4 => Scaler::Stretch,
        _ => Scaler::Auto,
    }
}

/// Display numbers that have a socket in `dir`, ascending.
///
/// A socket file is not proof that anyone is listening (stale sockets survive a
/// crash), which is why discovery connects and checks the pid instead of trusting
/// the directory listing.
pub fn display_numbers(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut numbers: Vec<u32> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_prefix('X')?.parse::<u32>().ok()
        })
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fsr(sharpness: u32) -> ScaleAlgorithm {
        ScaleAlgorithm::Fsr { sharpness }
    }

    #[test]
    fn runtime_settings_match_the_launch_arguments() {
        // -S fit -F fsr --sharpness (20 - internal * 4)
        let settings = Settings::for_algorithm(&fsr(2));
        assert_eq!(settings.filter, Filter::Fsr);
        assert_eq!(settings.scaler, Scaler::Fit);
        assert_eq!(settings.sharpness, 12);

        // -S integer -F nearest
        let settings = Settings::for_algorithm(&ScaleAlgorithm::Integer);
        assert_eq!(settings.filter, Filter::Nearest);
        assert_eq!(settings.scaler, Scaler::Integer);

        // -S fit -F linear
        let settings = Settings::for_algorithm(&ScaleAlgorithm::Bilinear);
        assert_eq!(settings.filter, Filter::Linear);
        assert_eq!(settings.scaler, Scaler::Fit);

        let settings = Settings::for_algorithm(&ScaleAlgorithm::Nis { sharpness: 5 });
        assert_eq!(settings.filter, Filter::Nis);
        assert_eq!(settings.scaler, Scaler::Fit);
        assert_eq!(settings.sharpness, 0);
    }

    #[test]
    fn toggling_fsr_twice_comes_back_to_where_it_started() {
        let start = Settings::for_algorithm(&fsr(2));
        let off = start.toggled_fsr();
        assert_eq!(off.filter, Filter::Linear);
        assert_eq!(off.toggled_fsr().filter, Filter::Fsr);
        // Sharpness survives the round trip: turning FSR off and on again must not
        // silently reset the setting the user chose at launch.
        assert_eq!(off.toggled_fsr().sharpness, start.sharpness);
    }

    #[test]
    fn toggling_nis_is_the_same_shape() {
        let start = Settings::for_algorithm(&fsr(2));
        assert_eq!(start.toggled_nis().filter, Filter::Nis);
        assert_eq!(start.toggled_nis().toggled_nis().filter, Filter::Linear);
    }

    #[test]
    fn nearest_and_bilinear_pick_a_scaler_too() {
        let start = Settings::for_algorithm(&fsr(2));
        assert_eq!(start.nearest().scaler, Scaler::Integer);
        assert_eq!(start.nearest().filter, Filter::Nearest);
        assert_eq!(start.bilinear().scaler, Scaler::Fit);
        assert_eq!(start.bilinear().filter, Filter::Linear);
    }

    #[test]
    fn sharpness_stops_at_gamescopes_limits() {
        // gamescope: 0 = sharpest, 20 = softest.
        let sharpest = Settings {
            sharpness: 0,
            ..Settings::for_algorithm(&fsr(0))
        };
        assert_eq!(sharpest.sharper().sharpness, 0);
        let softest = Settings {
            sharpness: GAMESCOPE_MAX_SHARPNESS,
            ..Settings::for_algorithm(&fsr(0))
        };
        assert_eq!(softest.softer().sharpness, GAMESCOPE_MAX_SHARPNESS);
        assert_eq!(softest.sharper().sharpness, GAMESCOPE_MAX_SHARPNESS - 1);
    }

    #[test]
    fn unknown_filter_values_are_reported_not_guessed() {
        assert!(filter_from_u32(9).is_err());
        assert_eq!(scaler_from_u32(9), Scaler::Auto);
    }

    #[test]
    fn socket_names_are_read_and_sorted() {
        let dir = std::env::temp_dir().join(format!("kotori-x11-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["X2", "X10", "X0", "not-a-display", "Xabc", "X1"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(display_numbers(&dir), vec![0, 1, 2, 10]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_socket_directory_is_not_an_error() {
        assert!(display_numbers(Path::new("/nonexistent/x11")).is_empty());
    }

    #[test]
    fn an_empty_socket_directory_finds_nothing() {
        let dir = std::env::temp_dir().join(format!("kotori-x11-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(GamescopeDisplay::discover_in(&dir, 1234).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Talking to the desktop environment.
//!
//! Only one thing lives here today: KDE's window control, which is how kotori
//! changes a running gamescope's output size — and with it the upscale ratio,
//! which is what "缩放" means to a user. Everything else in kotori talks to
//! gamescope, X11 or the portal directly, so this module stays deliberately small.

pub mod kde;

/// Is this a KDE Plasma session?
///
/// The window control below is Plasma-specific on purpose. On niri a window's size
/// is the layout's business (ADR-004), so "scale the game window" has nothing to
/// act on there, and the feature says so instead of pretending.
pub fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|desktop| desktop.to_ascii_lowercase().contains("kde"))
        .unwrap_or(false)
}

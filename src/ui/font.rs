//! Which font the window asks for.
//!
//! ADR-018: use the fonts the system already has — Microsoft YaHei UI and Segoe
//! Fluent Icons give the Windows 11 look for free — and **never ship
//! Microsoft's fonts**: this machine happens to have them under
//! `~/.local/share/fonts/win11/`, which is a local choice, not a redistribution.
//!
//! If none of the preferred families is installed we leave the window's default
//! alone instead of registering a bundled one. Slint 1.17 dropped the
//! `register_font_from_path`/`_from_static` entry points (runtime registration
//! now goes through the `unstable-fontique-010` feature), and it does per-glyph
//! fallback anyway: a machine without YaHei still renders Chinese through
//! whatever fontconfig offers.
//!
//! The 17MB `KotoriSans-Regular.ttf` that used to be bundled for exactly this
//! fallback was dropped from git history (nothing referenced it any more); a
//! local copy sits in the ignored `.font-backup/`. Shipping it again means
//! subsetting it *and* switching runtime registration on — see AGENTS.md P2-12.

use super::*;

/// The families to try, best first. The western half of YaHei UI is Segoe, so
/// one family covers both scripts.
const CANDIDATES: [&str; 4] = [
    "Microsoft YaHei UI",
    "Segoe UI Variable",
    "Noto Sans CJK SC",
    "Sarasa Gothic SC",
];

/// Point the window at the best font this machine actually has.
pub(super) fn install_font(window: &AppWindow) {
    match CANDIDATES.iter().find(|name| is_installed(name)) {
        Some(family) => {
            tracing::info!("UI 字体 {family}");
            window.set_ui_font((*family).into());
        }
        None => tracing::warn!(
            "系统里没有首选的中文/西文字体（{:?}），交给 Slint 的字体回退",
            CANDIDATES
        ),
    }
}

/// Whether fontconfig can resolve a family.
///
/// `fc-match` answers with a *substitute* when the family is missing, so the
/// only honest check is whether the first name it returns is the one we asked
/// for. Windows has no fontconfig — and always has YaHei — so it answers yes
/// there without asking.
fn is_installed(family: &str) -> bool {
    if cfg!(windows) {
        return true;
    }
    let output = match std::process::Command::new("fc-match")
        .args(["-f", "%{family}", family])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };
    let answer = String::from_utf8_lossy(&output.stdout);
    // A family list answers with several names ("Noto Sans CJK SC, Noto Sans…").
    answer
        .split(',')
        .next()
        .is_some_and(|name| name.trim() == family)
}

//! Window control for KDE Plasma, through KWin's scripting D-Bus interface.
//!
//! Why this, and not something simpler: a running gamescope is a **Wayland**
//! client of the compositor. Nothing about its size can be commanded over X11
//! (resizing another client's window has no meaning without a window manager
//! mediating), and Wayland deliberately gives one client no way to resize another
//! — the compositor is the only party allowed to do it. KWin's way in is that it
//! runs JavaScript on request.
//!
//! Measured on Plasma 6.5 / Wayland (2026-09-12), because every one of these was a
//! surprise at some point:
//!
//! * `w.frameGeometry = Object.assign({}, w.frameGeometry, {width, height})`
//!   resizes a **Wayland** window (dolphin 853x1035 → 640x480). Copying the
//!   existing geometry object matters: a fresh object literal is accepted and
//!   silently ignored. The resize is **asynchronous** — reading the geometry back
//!   immediately still shows the old size, so never verify that way.
//! * `w.fullScreen = true` works (the window becomes exactly the screen geometry).
//! * `w.output.devicePixelRatio` and `w.output.geometry` are readable, which is
//!   what lets a size be given in *physical* pixels and still land on whichever
//!   screen the window is on.
//! * `workspace.clientArea(...)` rejects every argument list we tried, so the
//!   clamping below uses the output geometry instead of the work area.
//! * Scripts are loaded under a stable plugin name so repeated presses replace
//!   one script rather than accumulating a hundred in the user's session.

use std::path::PathBuf;

use ashpd::zbus;

/// Anything that stops KWin from doing what we asked.
#[derive(Debug, thiserror::Error)]
pub enum KdeError {
    #[error("KWin 的 D-Bus 接口不可用（不是 KDE 会话？）：{0}")]
    Dbus(#[from] zbus::Error),
    #[error("KWin 脚本 {plugin} 写不进去：{source}")]
    Io {
        plugin: String,
        source: std::io::Error,
    },
    #[error("KWin 拒绝了脚本 {plugin}：{message}")]
    Refused { plugin: String, message: String },
}

const SERVICE: &str = "org.kde.KWin";
const SCRIPTING_IFACE: &str = "org.kde.kwin.Scripting";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPT_IFACE: &str = "org.kde.kwin.Script";

/// Resize the window owned by `pid` so the pixels it renders are `width x height`.
///
/// Physical pixels, not logical: a scale ratio is about pixels — the game renders
/// at its own resolution and gamescope fills `width x height` — while KWin's
/// geometry is logical. The script divides by the window's own
/// `devicePixelRatio`, and clamps the result to the screen, so a ratio that is too
/// large fills the screen instead of pushing the window off the edge.
pub async fn resize_window(pid: u32, width: u32, height: u32) -> Result<(), KdeError> {
    let body = format!(
        r#"
(() => {{
    const pid = {pid};
    const wantW = {width};
    const wantH = {height};
    let target = null;
    for (const w of workspace.windowList()) {{
        if (w.pid === pid) {{ target = w; break; }}
    }}
    if (target === null) {{
        print("kotori: 没有 pid 为 " + pid + " 的窗口");
        return;
    }}
    const out = target.output;
    const dpr = out ? out.devicePixelRatio : 1.0;
    const screen = out ? out.geometry : null;
    let q = Object.assign({{}}, target.frameGeometry);
    q.width = wantW / dpr;
    q.height = wantH / dpr;
    if (screen) {{
        // Never larger than the screen, never pushed off it: the two ways a
        // rescale could otherwise make a game unreachable.
        q.width = Math.min(q.width, screen.width);
        q.height = Math.min(q.height, screen.height);
        q.x = Math.max(screen.x, Math.min(q.x, screen.x + screen.width - q.width));
        q.y = Math.max(screen.y, Math.min(q.y, screen.y + screen.height - q.height));
    }}
    target.frameGeometry = q;
    print("kotori: pid " + pid + " → " + Math.round(q.width) + "x" + Math.round(q.height)
        + " 逻辑像素（" + wantW + "x" + wantH + " 物理像素，dpr " + dpr + "）");
}})();
"#
    );
    run("kotori-scale", &body).await
}

/// Flip the window's fullscreen state.
///
/// The direction is the compositor's to decide and remember — kotori keeps no copy
/// of it, so two presses always end up back where the user started.
pub async fn toggle_fullscreen(pid: u32) -> Result<(), KdeError> {
    let body = format!(
        r#"
(() => {{
    const pid = {pid};
    for (const w of workspace.windowList()) {{
        if (w.pid === pid) {{
            w.fullScreen = !w.fullScreen;
            print("kotori: pid " + pid + " 全屏 → " + w.fullScreen);
            return;
        }}
    }}
    print("kotori: 没有 pid 为 " + pid + " 的窗口");
}})();
"#
    );
    run("kotori-fullscreen", &body).await
}

/// Hand a script to KWin and run it.
///
/// One script per purpose is kept loaded (same plugin name each time), so a daemon
/// that lives for weeks — and a user who presses the key all evening — does not
/// leave anything behind.
async fn run(plugin: &str, body: &str) -> Result<(), KdeError> {
    let connection = zbus::Connection::session().await?;
    let scripting = zbus::Proxy::new(&connection, SERVICE, SCRIPTING_PATH, SCRIPTING_IFACE).await?;

    // Before loading: replacing the old script is what keeps the count at one.
    // A `false` here just means there was nothing to unload.
    let _: Result<bool, zbus::Error> = scripting.call("unloadScript", &(plugin,)).await;

    let path = write_script(plugin, body)?;
    let path = path.to_string_lossy().to_string();
    let id: i32 = scripting
        .call("loadScript", &(path.as_str(), plugin))
        .await
        .map_err(|err| KdeError::Refused {
            plugin: plugin.to_string(),
            message: err.to_string(),
        })?;

    let script_path = format!("/Scripting/Script{id}");
    let script = zbus::Proxy::new(&connection, SERVICE, script_path.as_str(), SCRIPT_IFACE).await?;
    let _: () = script.call("run", &()).await?;
    Ok(())
}

/// Write the script where KWin can read it.
///
/// It stays on disk: KWin reads the file when it loads the script, and keeping it
/// means the last thing kotori asked for is inspectable after the fact.
fn write_script(plugin: &str, body: &str) -> Result<PathBuf, KdeError> {
    let dir = crate::config::data_dir().join("kwin");
    let path = dir.join(format!("{plugin}.js"));
    let io = |source| KdeError::Io {
        plugin: plugin.to_string(),
        source,
    };
    std::fs::create_dir_all(&dir).map_err(io)?;
    std::fs::write(&path, body).map_err(io)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge itself, against the developer's own session.
    ///
    /// Ignored by default: CI has no KWin, and a test that needs a live desktop is
    /// not something `cargo test` should quietly require. Run it with
    /// `KOTORI_DATA_DIR=… cargo test -- --ignored kwin` — and point the data dir at
    /// a scratch path, because the daemon owns the real one.
    #[tokio::test]
    #[ignore = "needs a live KDE session"]
    async fn kwin_accepts_our_scripts() {
        run("kotori-selftest", r#"print("kotori: selftest");"#)
            .await
            .expect("KWin should accept a script");

        // No window has this pid, so KWin runs the script and prints that it found
        // nothing — which still has to come back as success: "no window" is only a
        // real answer once gamescope should have one.
        resize_window(4_242_424, 1600, 900)
            .await
            .expect("KWin should accept a resize script");
        toggle_fullscreen(4_242_424)
            .await
            .expect("KWin should accept a fullscreen script");
    }
}

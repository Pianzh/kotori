//! Wine 那两件事:**前缀**(哪来的、怎么找、怎么关)与**存档路径**(见 [`paths`])。
//!
//! 一分为二的理由:这两件事从来没有一起改过,而合在一份文件里的时候它是全仓最长的
//! 源文件(1179 行,`scripts/file-size-baseline.txt` 里的头名)。分界线就是
//! `home_dir` —— 它是唯一一个两边都要用的东西,所以留在这一层。
//!
//! 存档位置在配置里存的是**机器无关**的写法(见 [`crate::config::SavePathKind`]):
//! 前缀里的 Windows 风格路径、相对游戏根目录的路径,或者本机绝对路径。翻译与解析在
//! [`paths`],这里只管"该用哪个 prefix"。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::{Config, GameConfig};

#[cfg(test)]
mod path_tests;
mod paths;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use paths::*;

/// 这个 prefix 是从哪来的 —— 显示给用户看,免得"用的哪个"变成一个谜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixSource {
    /// `wine_prefix` of this game.
    Game,
    /// `[wine] prefix` in the config.
    Global,
    /// `$WINEPREFIX`.
    Environment,
    /// A prefix found inside the game directory (portable repacks).
    Portable(PathBuf),
    /// A prefix found in one of the well-known locations.
    Detected(PathBuf),
    /// The default `~/.wine` (created by wine on first use if needed).
    Default,
}

impl PrefixSource {
    pub fn label(&self) -> String {
        match self {
            Self::Game => "游戏自身的 wine prefix".to_string(),
            Self::Global => "全局配置的 wine prefix".to_string(),
            Self::Environment => "环境变量 WINEPREFIX".to_string(),
            Self::Portable(p) => format!("游戏目录内的可携式 prefix（{}）", p.display()),
            Self::Detected(p) => format!("自动探测到 {}", p.display()),
            Self::Default => "默认 ~/.wine".to_string(),
        }
    }
}

/// Well-known places that hold wine prefixes, relative to `$HOME`.
const KNOWN_PREFIX_DIRS: [&str; 4] = [
    ".local/share/wineprefixes",
    ".wine",
    "Games",
    ".local/share/bottles/bottles",
];

/// Resolve the prefix to use for a game, plus where that choice came from.
pub fn resolve_prefix(game: &GameConfig, config: &Config) -> (PathBuf, PrefixSource) {
    resolve_prefix_with(
        game,
        config,
        std::env::var_os("WINEPREFIX").map(PathBuf::from),
    )
}

/// Environment variable naming the `wineserver` binary, for installs that keep it
/// somewhere unusual and for tests.
pub const WINESERVER_ENV: &str = "KOTORI_WINESERVER";

/// How long `wineserver -k` gets to answer before kotori stops waiting for it.
///
/// It is a signal-and-exit helper: past this point, waiting longer only delays
/// the teardown it is a part of.
const WINESERVER_KILL_TIMEOUT: Duration = Duration::from_secs(2);

/// Shut down the wine server of **exactly one** prefix.
///
/// This is the only reliable way to make wine's `winedevice.exe` exit. Measured
/// on the real machine (2026-09-13): that process ignores `SIGTERM`, so
/// signalling it — or the process group it puts itself in, or the tree it hangs
/// off — leaves it behind, and a left-behind `winedevice.exe` keeps whatever
/// systemd scope it landed in alive for the whole 90 s `TimeoutStopSec`. That
/// happened twice, and each time it was 90 s added to a shutdown.
/// `wineserver -k` terminates every process attached to that prefix's server in
/// one step, instead of chasing them one at a time.
///
/// Deliberately one prefix, and never `pkill wineserver`: a machine holds several
/// prefixes, and killing the wrong server would take down a game kotori does not
/// own.
///
/// Call it *after* the game's own processes are gone, never before — while the
/// game is running, `wineserver -k` would be killing a live game's server.
/// Failures are logged and swallowed: a teardown that can fail is a teardown that
/// leaves behind the mess it was called to remove.
pub async fn close_prefix(prefix: &Path) {
    close_prefix_with(&wineserver_binary(), prefix).await;
}

/// The `wineserver` kotori is going to run.
///
/// `KOTORI_WINESERVER` names it explicitly (tests point it at a fake), which is also
/// how the shutdown sweep in [`crate::wine_prefixes`] gets the same binary without
/// repeating the lookup.
pub fn wineserver_binary() -> PathBuf {
    std::env::var_os(WINESERVER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wineserver"))
}

/// [`close_prefix`] with the binary named explicitly.
///
/// The binary and the prefix are both parameters, and `WINEPREFIX` is passed to
/// the child rather than read from our own environment, so this can be tested
/// without any test mutating the process environment — which would race with
/// every other test thread.
pub async fn close_prefix_with(binary: &Path, prefix: &Path) {
    let mut child = match tokio::process::Command::new(binary)
        .arg("-k")
        .env("WINEPREFIX", prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(
                "起不了 {}（{err}）；{} 里的 wine 残留要留到下次重启了",
                binary.display(),
                prefix.display()
            );
            return;
        }
    };

    match tokio::time::timeout(WINESERVER_KILL_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => {
            tracing::debug!("wineserver -k 收掉了 {}", prefix.display());
        }
        Ok(Ok(status)) => tracing::debug!(
            "wineserver -k 对 {} 退出码 {status}（通常表示本来就没有 server 在跑）",
            prefix.display()
        ),
        Ok(Err(err)) => tracing::warn!("等 wineserver -k 出错：{err}"),
        Err(_) => {
            let _ = child.start_kill();
            tracing::warn!(
                "wineserver -k 超过 {}s 没返回，不再等它（{}）",
                WINESERVER_KILL_TIMEOUT.as_secs(),
                prefix.display()
            );
        }
    }
}

/// [`resolve_prefix`] with the environment lookup passed in.
///
/// Kept separate so tests can exercise the `WINEPREFIX` branch without mutating
/// the process environment, which would race with every other test thread.
pub fn resolve_prefix_with(
    game: &GameConfig,
    config: &Config,
    wineprefix_env: Option<PathBuf>,
) -> (PathBuf, PrefixSource) {
    if let Some(prefix) = &game.wine_prefix
        && !prefix.as_os_str().is_empty()
    {
        return (prefix.clone(), PrefixSource::Game);
    }

    if let Some(prefix) = &config.wine.prefix
        && !prefix.as_os_str().is_empty()
    {
        return (prefix.clone(), PrefixSource::Global);
    }

    if let Some(prefix) = wineprefix_env
        && !prefix.as_os_str().is_empty()
    {
        return (prefix, PrefixSource::Environment);
    }

    let game_dir = game.effective_game_dir();
    if let Some(portable) = portable_prefix(&game_dir) {
        return (portable.clone(), PrefixSource::Portable(portable));
    }

    if let Some(found) = detect_prefixes(&game_dir).into_iter().next() {
        return (found.clone(), PrefixSource::Detected(found));
    }

    (default_prefix(), PrefixSource::Default)
}

/// `~/.wine`.
pub fn default_prefix() -> PathBuf {
    home_dir().join(".wine")
}

/// A prefix shipped inside the game directory (common in repacks).
fn portable_prefix(game_dir: &Path) -> Option<PathBuf> {
    for candidate in [game_dir.join("drive_c"), game_dir.join("prefix")] {
        let is_prefix = if candidate.ends_with("drive_c") {
            candidate.is_dir()
        } else {
            candidate.join("drive_c").is_dir()
        };
        if is_prefix {
            // For `<dir>/drive_c` the prefix *is* `<dir>`.
            return Some(if candidate.ends_with("drive_c") {
                candidate.parent()?.to_path_buf()
            } else {
                candidate
            });
        }
    }
    None
}

/// Wine prefixes that exist in the usual locations (newest-agnostic order).
///
/// Scans the well-known directories for a `drive_c` inside each child, which is
/// what actually identifies a prefix.
pub fn detect_prefixes(game_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if let Some(portable) = portable_prefix(game_dir) {
        found.push(portable);
    }

    let home = home_dir();
    for relative in KNOWN_PREFIX_DIRS {
        let root = home.join(relative);
        if !root.is_dir() {
            continue;
        }
        // `~/.wine` is itself a prefix; the others hold prefixes as children.
        if root.join("drive_c").is_dir() {
            found.push(root.clone());
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("drive_c").is_dir() || p.join("pfx/drive_c").is_dir())
                .collect();
            children.sort();
            for child in children {
                let prefix = if child.join("drive_c").is_dir() {
                    child
                } else {
                    child.join("pfx")
                };
                found.push(prefix);
            }
        }
    }

    found.dedup();
    found
}

/// 本机用户目录;认不出来就退回当前目录(`dirs` 在某些容器里没有答案)。
fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

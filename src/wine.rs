//! Wine prefix discovery and save-path resolution.
//!
//! Save locations are stored in a machine-independent form (see
//! [`crate::config::SavePathKind`]): a Windows-style path inside the prefix, a
//! path relative to the game root, or an absolute path. This module turns that
//! description into a real path on this machine, and finds the wine prefix to
//! use when the config does not name one.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::{Config, GameConfig, SavePath, SavePathKind};

/// Windows environment tokens we understand, and the prefix-relative path each
/// one maps to. Windows and wine agree on these, which is what makes a save
/// location portable between the two platforms.
pub const TOKENS: [(&str, &str); 5] = [
    ("%APPDATA%", "AppData/Roaming"),
    ("%LOCALAPPDATA%", "AppData/Local"),
    ("%USERPROFILE%", ""),
    ("%DOCUMENTS%", "Documents"),
    ("%SAVEDGAMES%", "Saved Games"),
];

/// Where a wine prefix came from — shown in the UI so the choice is never a
/// mystery.
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
    let binary = std::env::var_os(WINESERVER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wineserver"));
    close_prefix_with(&binary, prefix).await;
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

/// What Windows-style save paths are resolved against.
///
/// The same configuration has to work on both sides of a dual boot, where the
/// "user profile" is a completely different thing: inside a wine prefix it is
/// `drive_c/users/<name>`, on real Windows it is `%USERPROFILE%`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveRoot {
    /// Linux, running the game through wine.
    WinePrefix(PathBuf),
    /// Real Windows: the user profile directory.
    UserProfile(PathBuf),
}

impl SaveRoot {
    /// The root that applies to the platform this build runs on.
    pub fn for_platform(game: &GameConfig, config: &Config) -> (Self, PrefixSource) {
        if cfg!(windows) {
            let profile = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(home_dir);
            // Windows has no prefix at all; the label just says so.
            return (Self::UserProfile(profile), PrefixSource::Default);
        }
        let (prefix, source) = resolve_prefix(game, config);
        (Self::WinePrefix(prefix), source)
    }
}

/// Turn a [`SavePath`] into a real path on this machine.
///
/// Returns `Err` with a user-facing message when the description cannot be
/// resolved (unknown token, or a drive path that is not inside the profile).
pub fn resolve_save_path(
    root: &SaveRoot,
    game_dir: &Path,
    save: &SavePath,
) -> Result<PathBuf, String> {
    match save.kind {
        SavePathKind::Relative => {
            let relative = save.path.trim();
            if relative.is_empty() {
                return Err("相对路径不能为空".to_string());
            }
            Ok(game_dir.join(relative))
        }
        // Absolute paths are device-local by definition; `~` only means
        // something on Linux, so on Windows it is passed through untouched.
        SavePathKind::Absolute => Ok(expand_home(save.path.trim())),
        SavePathKind::Windows => resolve_windows_path(root, &save.path),
    }
}

/// Resolve a Windows-style path (`%APPDATA%\Game\save` or `C:\users\x\...`)
/// against the root of whichever platform we are on.
fn resolve_windows_path(root: &SaveRoot, path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Windows 路径不能为空".to_string());
    }

    if let Some(rest) = strip_drive_letter(trimmed) {
        return match root {
            // Inside a prefix every literal drive path is relative to `drive_c`.
            SaveRoot::WinePrefix(prefix) => Ok(join_windows(&prefix.join("drive_c"), rest)),
            // On real Windows only paths inside the user profile are portable;
            // anything else would be a machine-specific absolute path wearing a
            // Windows costume.
            SaveRoot::UserProfile(profile) => {
                let rest = rest.trim_start_matches(['\\', '/']);
                let segments: Vec<&str> = rest.split(['\\', '/']).collect();
                let looks_like_profile =
                    segments.len() >= 3 && segments[0].eq_ignore_ascii_case("users");
                if !looks_like_profile {
                    return Err(format!(
                        "在 Windows 上请用 %APPDATA% / %USERPROFILE% 这类令牌，而不是 {trimmed}\
                         （只有用户目录内的路径才能跨设备对应）"
                    ));
                }
                Ok(join_windows(profile, &segments[2..].join("\\")))
            }
        };
    }

    let upper = trimmed.to_uppercase();
    let Some((token, mapped)) = TOKENS.iter().find(|(t, _)| upper.starts_with(t)) else {
        return Err(format!(
            "无法识别的路径令牌: {trimmed}（可用: {}）",
            TOKENS
                .iter()
                .map(|(t, _)| *t)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let rest = &trimmed[token.len()..];
    let rest = rest.trim_start_matches(['\\', '/']);

    // Same token, same meaning on both platforms — this is what lets one
    // configuration describe a save location for Linux *and* Windows.
    let user_dir = match root {
        SaveRoot::WinePrefix(prefix) => windows_user_dir(prefix),
        SaveRoot::UserProfile(profile) => profile.clone(),
    };

    Ok(if mapped.is_empty() {
        join_windows(&user_dir, rest)
    } else {
        join_windows(&user_dir.join(mapped), rest)
    })
}

/// `C:\foo\bar` -> `Some("foo\bar")`; anything else -> `None`.
fn strip_drive_letter(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        Some(path[2..].trim_start_matches(['\\', '/']))
    } else {
        None
    }
}

/// Join a Windows-style relative path onto a base directory.
fn join_windows(base: &Path, relative: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for segment in relative.split(['\\', '/']) {
        let segment = segment.trim();
        if !segment.is_empty() && segment != "." {
            path.push(segment);
        }
    }
    path
}

/// The Windows user directory inside a prefix (`drive_c/users/<user>`).
///
/// The name depends on how the prefix was made: plain wine uses the Linux
/// account, Proton usually `steamuser`. Prefer the current user, then any real
/// user directory, then assume the Linux user name.
pub fn windows_user_dir(prefix: &Path) -> PathBuf {
    let users = prefix.join("drive_c").join("users");
    let current = std::env::var("USER").unwrap_or_default();

    let candidates: Vec<PathBuf> = std::fs::read_dir(&users)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();

    if !current.is_empty()
        && let Some(found) = candidates
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n == current.as_str()))
    {
        return found.clone();
    }

    let mut usable: Vec<&PathBuf> = candidates
        .iter()
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| matches!(n, "Public" | "Default" | "Default User" | "All Users"))
        })
        .collect();
    usable.sort();
    if let Some(found) = usable.first() {
        return (*found).clone();
    }

    if current.is_empty() {
        users.join("user")
    } else {
        users.join(current)
    }
}

/// Expand a leading `~`.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else if path == "~" {
        home_dir()
    } else {
        PathBuf::from(path)
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GameConfig, SavePathKind, ScaleProfile};

    /// Build a throw-away wine prefix with the given user directories.
    struct FakePrefix(PathBuf);

    impl FakePrefix {
        fn new(tag: &str, users: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-wine-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let users_dir = dir.join("drive_c").join("users");
            for user in users {
                std::fs::create_dir_all(users_dir.join(user)).unwrap();
            }
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for FakePrefix {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn game(game_dir: &str, exe: &str) -> GameConfig {
        GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from(game_dir),
            exe_path: PathBuf::from(exe),
            launch_args: Vec::new(),
            save_paths: Vec::new(),
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn infers_the_storage_kind_from_a_bare_path() {
        assert_eq!(
            SavePathKind::infer("%APPDATA%\\X\\save"),
            SavePathKind::Windows
        );
        assert_eq!(
            SavePathKind::infer("C:\\users\\a\\save"),
            SavePathKind::Windows
        );
        assert_eq!(SavePathKind::infer("d:/save"), SavePathKind::Windows);
        assert_eq!(SavePathKind::infer("savedata"), SavePathKind::Relative);
        assert_eq!(SavePathKind::infer("save/"), SavePathKind::Relative);
        assert_eq!(SavePathKind::infer("/opt/saves/x"), SavePathKind::Absolute);
        assert_eq!(SavePathKind::infer("~/saves/x"), SavePathKind::Absolute);
    }

    #[test]
    fn resolves_tokens_inside_the_prefix() {
        let prefix = FakePrefix::new("tokens", &["tester"]);
        let base = prefix.path().join("drive_c/users/tester");

        for (input, expected) in [
            (
                "%APPDATA%\\Game\\save",
                base.join("AppData/Roaming/Game/save"),
            ),
            ("%LOCALAPPDATA%\\Game", base.join("AppData/Local/Game")),
            ("%DOCUMENTS%\\Game", base.join("Documents/Game")),
            ("%SAVEDGAMES%\\Game", base.join("Saved Games/Game")),
            ("%USERPROFILE%\\Game", base.join("Game")),
        ] {
            let save = SavePath::inferred(input);
            assert_eq!(
                resolve_save_path(
                    &SaveRoot::WinePrefix(prefix.path()),
                    Path::new("/games/demo"),
                    &save
                )
                .unwrap(),
                expected,
                "for {input}"
            );
        }
    }

    #[test]
    fn resolves_literal_drive_paths_and_mixed_separators() {
        let prefix = FakePrefix::new("drive", &["tester"]);
        let saved = resolve_save_path(
            &SaveRoot::WinePrefix(prefix.path()),
            Path::new("/games/demo"),
            &SavePath::new(SavePathKind::Windows, "C:/users/tester/Documents/Game"),
        )
        .unwrap();
        assert_eq!(
            saved,
            prefix.path().join("drive_c/users/tester/Documents/Game")
        );
    }

    #[test]
    fn unknown_tokens_are_reported_not_guessed() {
        let prefix = FakePrefix::new("bad-token", &["tester"]);
        let err = resolve_save_path(
            &SaveRoot::WinePrefix(prefix.path()),
            Path::new("/games/demo"),
            &SavePath::new(SavePathKind::Windows, "%PROGRAMFILES%\\Game"),
        )
        .unwrap_err();
        assert!(err.contains("%PROGRAMFILES%"), "{err}");
        assert!(
            err.contains("%APPDATA%"),
            "the message should list the tokens"
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_game_root() {
        let save = SavePath::new(SavePathKind::Relative, "savedata");
        assert_eq!(
            resolve_save_path(
                &SaveRoot::WinePrefix("/prefix".into()),
                Path::new("/games/demo"),
                &save
            )
            .unwrap(),
            PathBuf::from("/games/demo/savedata")
        );
    }

    #[test]
    fn absolute_paths_stay_put_and_expand_home() {
        let save = SavePath::new(SavePathKind::Absolute, "/opt/saves/demo");
        assert_eq!(
            resolve_save_path(
                &SaveRoot::WinePrefix("/prefix".into()),
                Path::new("/games/demo"),
                &save
            )
            .unwrap(),
            PathBuf::from("/opt/saves/demo")
        );

        let home = SavePath::new(SavePathKind::Absolute, "~/saves/demo");
        let resolved = resolve_save_path(
            &SaveRoot::WinePrefix("/prefix".into()),
            Path::new("/games/demo"),
            &home,
        )
        .unwrap();
        assert!(resolved.ends_with("saves/demo"));
        assert!(!resolved.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn the_same_token_resolves_on_both_sides_of_a_dual_boot() {
        // One configuration, two systems: inside a wine prefix the token lands
        // under drive_c, on real Windows under the user profile. This equality
        // of *meaning* is what makes a save location portable.
        let prefix = FakePrefix::new("dual", &["tester"]);
        let save = SavePath::new(SavePathKind::Windows, "%APPDATA%\\Game\\save");

        let on_linux = resolve_save_path(
            &SaveRoot::WinePrefix(prefix.path()),
            Path::new("/games/demo"),
            &save,
        )
        .unwrap();
        assert_eq!(
            on_linux,
            prefix
                .path()
                .join("drive_c/users/tester/AppData/Roaming/Game/save")
        );

        let on_windows = resolve_save_path(
            &SaveRoot::UserProfile("C:/Users/tester".into()),
            Path::new("C:/games/demo"),
            &save,
        )
        .unwrap();
        assert_eq!(
            on_windows,
            PathBuf::from("C:/Users/tester/AppData/Roaming/Game/save")
        );

        // Both describe the same place, which is the whole point.
        assert_eq!(
            on_linux
                .strip_prefix(prefix.path().join("drive_c/users/tester"))
                .unwrap(),
            on_windows.strip_prefix("C:/Users/tester").unwrap()
        );
    }

    #[test]
    fn windows_only_accepts_portable_drive_paths() {
        let save = SavePath::new(SavePathKind::Windows, "C:\\users\\tester\\Documents\\Game");
        // Inside a prefix any drive path is fine (it is one machine's prefix).
        let in_prefix = resolve_save_path(
            &SaveRoot::WinePrefix("/prefix".into()),
            Path::new("/games/demo"),
            &save,
        )
        .unwrap();
        assert_eq!(
            in_prefix,
            PathBuf::from("/prefix/drive_c/users/tester/Documents/Game")
        );

        // On real Windows a path outside the profile is rejected: it would be a
        // machine-specific absolute path pretending to be portable.
        let on_windows = resolve_save_path(
            &SaveRoot::UserProfile("C:/Users/tester".into()),
            Path::new("C:/games/demo"),
            &save,
        )
        .unwrap();
        assert_eq!(on_windows, PathBuf::from("C:/Users/tester/Documents/Game"));

        let bad = SavePath::new(SavePathKind::Windows, "C:\\Program Files\\Game\\save");
        let error = resolve_save_path(
            &SaveRoot::UserProfile("C:/Users/tester".into()),
            Path::new("C:/games/demo"),
            &bad,
        )
        .unwrap_err();
        assert!(error.contains("令牌"), "{error}");
    }

    #[test]
    fn picks_the_current_user_directory_first() {
        let prefix = FakePrefix::new("users", &["steamuser", "Public"]);
        let user_dir = windows_user_dir(&prefix.path());
        // `steamuser` is the only real account here, `Public` is never a game's home.
        assert!(user_dir.ends_with("users/steamuser"), "{user_dir:?}");
    }

    #[test]
    fn portable_prefix_inside_the_game_dir_wins_over_the_default() {
        let dir = std::env::temp_dir().join(format!("kotori-portable-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("drive_c/users/tester")).unwrap();

        let config = Config::default();
        let game = game(
            &dir.to_string_lossy(),
            &format!("{}/game.exe", dir.display()),
        );
        let (prefix, source) = resolve_prefix(&game, &config);

        assert_eq!(prefix, dir);
        assert!(matches!(source, PrefixSource::Portable(_)), "{source:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn per_game_prefix_beats_the_global_one() {
        let mut config = Config::default();
        config.wine.prefix = Some(PathBuf::from("/global/prefix"));

        let mut with_own = game("/games/demo", "/games/demo/game.exe");
        with_own.wine_prefix = Some(PathBuf::from("/game/prefix"));
        assert_eq!(
            resolve_prefix(&with_own, &config),
            (PathBuf::from("/game/prefix"), PrefixSource::Game)
        );

        let inherited = game("/games/demo", "/games/demo/game.exe");
        assert_eq!(
            resolve_prefix(&inherited, &config),
            (PathBuf::from("/global/prefix"), PrefixSource::Global)
        );
    }

    #[test]
    fn falls_back_to_the_default_prefix() {
        let config = Config::default();
        let game = game("/nonexistent/game", "/nonexistent/game/game.exe");
        // No WINEPREFIX, no portable prefix, nothing detected.
        let (_, source) = resolve_prefix_with(&game, &config, None);
        assert!(
            matches!(source, PrefixSource::Default | PrefixSource::Detected(_)),
            "{source:?}"
        );
    }

    #[test]
    fn the_environment_prefix_is_used_when_nothing_else_is() {
        let config = Config::default();
        let game = game("/nonexistent/game", "/nonexistent/game/game.exe");
        let (prefix, source) =
            resolve_prefix_with(&game, &config, Some(PathBuf::from("/env/prefix")));
        assert_eq!(prefix, PathBuf::from("/env/prefix"));
        assert_eq!(source, PrefixSource::Environment);

        // An empty value is ignored rather than becoming an empty prefix.
        let (_, source) = resolve_prefix_with(&game, &config, Some(PathBuf::from("")));
        assert_ne!(source, PrefixSource::Environment);
    }

    #[test]
    fn save_path_accepts_both_config_forms() {
        #[derive(serde::Deserialize)]
        struct Holder {
            save_paths: Vec<SavePath>,
        }

        let toml = r#"
save_paths = [
  "savedata",
  "%APPDATA%\\Game\\save",
  { kind = "absolute", path = "/opt/saves/demo", exclude = ["*.log"] },
]
"#;
        let holder: Holder = toml::from_str(toml).unwrap();
        assert_eq!(holder.save_paths[0].kind, SavePathKind::Relative);
        assert_eq!(holder.save_paths[1].kind, SavePathKind::Windows);
        assert_eq!(holder.save_paths[2].kind, SavePathKind::Absolute);
        assert_eq!(holder.save_paths[2].exclude, ["*.log"]);

        // Round-trips back out as explicit tables.
        let mut config = Config::default();
        config.games.insert(
            "demo".into(),
            GameConfig {
                save_paths: holder.save_paths,
                ..game("/games/demo", "/games/demo/game.exe")
            },
        );
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("kind = \"windows\""), "{text}");
        let reparsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(reparsed.games["demo"].save_paths[2].exclude, ["*.log"]);
    }

    /// A scratch directory that names itself, so parallel tests never share one.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kotori-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake `wineserver` that writes down what it was asked to do.
    fn recording_wineserver(dir: &Path) -> PathBuf {
        let script = dir.join("wineserver");
        let log = dir.join("call.txt");
        crate::secrets::testing::write_executable(
            &script,
            &format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--kotori-warmup\" ]; then exit 0; fi\n\
                 echo \"args=$* prefix=$WINEPREFIX\" > {}\n",
                log.display()
            ),
        );
        script
    }

    #[tokio::test]
    async fn closing_a_prefix_names_that_prefix_to_wineserver() {
        let dir = scratch("wineserver-args");
        let script = recording_wineserver(&dir);
        let prefix = dir.join("prefix");

        close_prefix_with(&script, &prefix).await;

        let recorded = std::fs::read_to_string(dir.join("call.txt")).unwrap();
        assert!(recorded.contains("args=-k"), "{recorded}");
        assert!(
            recorded.contains(&prefix.display().to_string()),
            "the prefix must be the one we were asked to close: {recorded}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_missing_wineserver_is_survivable() {
        // This runs while a session is being torn down, so "wine is not where we
        // thought it was" must not turn into a failed teardown or a panic.
        let missing = scratch("wineserver-missing").join("nope");
        close_prefix_with(&missing, Path::new("/games/demo/prefix")).await;
    }

    #[tokio::test]
    async fn a_wineserver_that_never_answers_does_not_hold_up_the_teardown() {
        // The whole point of the timeout: a wedged helper must not become a
        // wedged shutdown of its own.
        let dir = scratch("wineserver-hang");
        let script = dir.join("wineserver");
        crate::secrets::testing::write_executable(
            &script,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--kotori-warmup\" ]; then exit 0; fi\n\
             exec sleep 60\n",
        );

        let started = std::time::Instant::now();
        close_prefix_with(&script, Path::new("/games/demo/prefix")).await;

        let waited = started.elapsed();
        assert!(
            waited < WINESERVER_KILL_TIMEOUT * 3,
            "waited {waited:?}, which means the timeout did not fire"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

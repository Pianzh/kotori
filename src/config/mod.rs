use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
mod paths;
mod profile;

// Only what the rest of the crate actually names. Everything else stays reachable
// through `config::profile` / `config::paths` — a re-export nobody uses is a
// warning, and a re-export list that lies about the surface is worse.
pub use paths::{
    config_path, data_dir, default_socket_path, load, load_at, log_dir, plain_secrets_path,
    resolve_socket, save, save_to, secrets_path, socket_path,
};
pub use profile::{
    FALLBACK_OUTPUT_HEIGHT, FALLBACK_OUTPUT_WIDTH, MAX_SHARPNESS, ScaleAlgorithm, ScaleProfile,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Machine-wide wine settings; a game can override the prefix.
    #[serde(default)]
    pub wine: WineConfig,
    /// Cloud save sync (Phase 2).
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub games: HashMap<String, GameConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

/// Machine-wide wine settings shared by all games.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WineConfig {
    /// Wine prefix used when a game does not name its own. `None` means
    /// "auto-detect" (see [`crate::wine::resolve_prefix`]).
    #[serde(default)]
    pub prefix: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfig {
    pub name: String,
    /// Where the game is installed. This is the working directory a launch
    /// uses (many VNs resolve assets relative to it) and the base that
    /// [`SavePathKind::Relative`] save paths resolve against.
    ///
    /// Older configs have no such field; [`Config::normalize`] fills it in
    /// from the exe's parent directory.
    #[serde(default)]
    pub game_dir: PathBuf,
    pub exe_path: PathBuf,
    /// Extra arguments passed to the exe.
    #[serde(default)]
    pub launch_args: Vec<String>,
    /// Where this game keeps its saves. Empty means "not configured yet".
    #[serde(default)]
    pub save_paths: Vec<SavePath>,
    /// Per-game wine prefix; overrides the global [`WineConfig::prefix`].
    #[serde(default)]
    pub wine_prefix: Option<PathBuf>,
    /// kotori never launches this game (the user starts it themselves, or a
    /// launcher does). Save sync still works by watching `process_name`.
    #[serde(default)]
    pub watch_only: bool,
    /// Process name to watch so save sync knows when the game is running.
    /// Useful for launcher games (where the launched process exits early) and
    /// required for watch-only games.
    #[serde(default)]
    pub process_name: Option<String>,
    pub scale_profile: ScaleProfile,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Cloud save sync settings.
///
/// **Nothing secret is stored here.** The B2 credentials and the sync password
/// live in the OS keyring (see [`crate::secrets`]), which encrypts them at rest
/// while still letting their owner read them back with standard tooling. This
/// struct only holds the non-sensitive settings (ADR-010).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Optional override for the storage API endpoint.
    ///
    /// Normally **empty**: rclone's native `b2` backend discovers the right
    /// regional API host from the credentials itself, and every B2 account
    /// works that way. Set it only to pin a specific endpoint, as a full URL
    /// (`https://api001.backblazeb2.com`) — a bare host does not work.
    ///
    /// Note this is *not* the `s3.<region>.backblazeb2.com` value the B2
    /// console shows: that is the S3-compatible API, a different service this
    /// backend does not speak. [`crate::sync::validate`] rejects it by name
    /// rather than letting rclone fail with a confusing 404.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub bucket: String,
    /// Folder inside the bucket that kotori owns.
    #[serde(default = "default_sync_prefix")]
    pub prefix: String,
    /// Wrap the data in rclone's `crypt` layer. Off by default: without it the
    /// saves are plain files, readable without kotori *and* without rclone.
    #[serde(default)]
    pub encryption: bool,
    /// Version snapshots kept per save location; `0` keeps all of them, which
    /// is the default — silently dropping an old save is worse than using space.
    #[serde(default)]
    pub keep_versions: u32,
}

fn default_sync_prefix() -> String {
    "kotori".to_string()
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            bucket: String::new(),
            prefix: default_sync_prefix(),
            encryption: false,
            keep_versions: 0,
        }
    }
}

/// How a save path is interpreted. The three kinds exist so that the same
/// configuration can be understood on Linux (under wine) and on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavePathKind {
    /// A Windows-style path inside the wine prefix, e.g.
    /// `%APPDATA%\Game\save`. Environment tokens are preferred over a literal
    /// `C:\users\<name>\...` because the user name differs between prefixes
    /// (plain wine uses the Linux account, Proton usually `steamuser`).
    Windows,
    /// Relative to [`GameConfig::game_dir`].
    Relative,
    /// Absolute path on this machine: not portable, never mapped to Windows.
    Absolute,
}

impl SavePathKind {
    /// Guess the kind from a bare path string, following the project rule:
    /// Windows-looking paths stay Windows, absolute stays absolute, everything
    /// else is relative to the game root.
    pub fn infer(path: &str) -> Self {
        let trimmed = path.trim();
        if trimmed.starts_with('%') || is_windows_absolute(trimmed) {
            Self::Windows
        } else if trimmed.starts_with('/') || trimmed.starts_with('~') {
            Self::Absolute
        } else {
            Self::Relative
        }
    }
}

/// `C:\...` / `c:/...` — a Windows drive path.
fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// One save location of a game.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavePath {
    pub kind: SavePathKind,
    pub path: String,
    /// Glob patterns (relative to this path) that must not be synced, e.g.
    /// `*.log` or `cache/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl SavePath {
    pub fn new(kind: SavePathKind, path: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            exclude: Vec::new(),
        }
    }

    /// Build from a bare string, inferring the kind.
    pub fn inferred(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(SavePathKind::infer(&path), path)
    }

    /// One-line description for CLI output.
    pub fn describe(&self) -> String {
        let kind = match self.kind {
            SavePathKind::Windows => "windows 路径",
            SavePathKind::Relative => "相对游戏目录",
            SavePathKind::Absolute => "绝对路径（仅本机）",
        };
        if self.exclude.is_empty() {
            format!("{}（{}）", self.path, kind)
        } else {
            format!(
                "{}（{}，排除 {}）",
                self.path,
                kind,
                self.exclude.join(", ")
            )
        }
    }
}

/// Accept both the compact form (`"savedata"`, `"%APPDATA%\\Game"`) and the
/// explicit table form (`{ kind = "windows", path = "...", exclude = [...] }`),
/// so hand-written configs stay short without losing precision when needed.
impl<'de> Deserialize<'de> for SavePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Detailed {
            #[serde(default)]
            kind: Option<SavePathKind>,
            path: String,
            #[serde(default)]
            exclude: Vec<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Simple(String),
            Detailed(Detailed),
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Simple(path) => SavePath::inferred(path),
            Repr::Detailed(d) => SavePath {
                kind: d.kind.unwrap_or_else(|| SavePathKind::infer(&d.path)),
                path: d.path,
                exclude: d.exclude,
            },
        })
    }
}

impl Config {
    /// Fill in what older (or freshly hand-written) configs leave out, so the
    /// rest of the code can rely on the invariants. Runs on every load, and
    /// the result is written back the next time the config is saved.
    pub fn normalize(&mut self) {
        for game in self.games.values_mut() {
            game.normalize();
        }
    }
}

impl GameConfig {
    /// See [`Config::normalize`].
    pub fn normalize(&mut self) {
        if self.game_dir.as_os_str().is_empty()
            && let Some(parent) = self.exe_path.parent()
        {
            self.game_dir = parent.to_path_buf();
        }
    }

    /// Working directory of a launch, and the base for relative save paths.
    pub fn effective_game_dir(&self) -> PathBuf {
        if self.game_dir.as_os_str().is_empty() {
            self.exe_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        } else {
            self.game_dir.clone()
        }
    }

    /// Can kotori launch this game itself, or is it watch-only?
    pub fn is_launchable(&self) -> bool {
        !self.watch_only
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            log_level: default_log_level(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kotori-test-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    fn sample_config() -> Config {
        let mut config = Config::default();
        config.games.insert(
            "demo".into(),
            GameConfig {
                name: "demo".into(),
                game_dir: PathBuf::from("/games/demo"),
                exe_path: PathBuf::from("/games/demo/game.exe"),
                launch_args: Vec::new(),
                watch_only: false,
                process_name: None,
                save_paths: vec![SavePath::inferred("%APPDATA%\\Demo\\save")],
                scale_profile: ScaleProfile {
                    algorithm: ScaleAlgorithm::Nis { sharpness: 4 },
                    framerate_limit: Some(60),
                    ..ScaleProfile::default_for((2560, 1440))
                },
                wine_prefix: None,
                created_at: chrono::Utc::now(),
            },
        );
        config
    }

    #[test]
    fn config_round_trips_through_toml() {
        let path = temp_path("roundtrip");
        let config = sample_config();

        save_to(&path, &config).unwrap();
        let loaded = paths::load_from(&path).unwrap();

        assert_eq!(loaded.games.len(), 1);
        let game = &loaded.games["demo"];
        assert_eq!(
            game.scale_profile.algorithm,
            ScaleAlgorithm::Nis { sharpness: 4 }
        );
        assert_eq!(game.scale_profile.framerate_limit, Some(60));
        assert_eq!(game.scale_profile.output_width, 2560);
        assert!(game.scale_profile.force_fullscreen);
        assert_eq!(game.save_paths.len(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let path = temp_path("missing").with_file_name("does-not-exist.toml");
        assert!(!path.exists());
        // load_from is strict; the lenient behaviour lives in `load()` and is
        // covered by `corrupt_config_is_backed_up`.
        assert!(paths::load_from(&path).is_err());
    }

    #[test]
    fn corrupt_config_is_backed_up() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "this is not = valid toml {{{").unwrap();

        let parsed = paths::load_from(&path);
        assert!(parsed.is_err());

        // Emulate `load()`'s backup step without touching the real config path.
        let backup = path.with_extension("toml.corrupt");
        std::fs::rename(&path, &backup).unwrap();
        assert!(backup.exists());
        assert!(!path.exists());

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn legacy_lanczos_config_no_longer_parses() {
        // Lanczos was never a real gamescope filter; documents the intentional
        // break so a stale config surfaces loudly instead of silently bilinear.
        let toml = r#"
[daemon]
socket_path = "/tmp/kotori.sock"
log_level = "info"

[games.old]
name = "old"
exe_path = "/games/old/game.exe"
save_paths = []
created_at = "2026-01-01T00:00:00Z"

[games.old.scale_profile]
name = "默认"
internal_width = 1280
internal_height = 720
output_width = 2560
output_height = 1440
force_fullscreen = true
algorithm = "Lanczos"
"#;
        assert!(toml::from_str::<Config>(toml).is_err());
    }

    #[test]
    fn partial_game_config_uses_serde_defaults() {
        // Missing optional fields must not break loading an older config.
        let toml = r#"
[games.minimal]
name = "minimal"
exe_path = "/games/minimal/game.exe"
created_at = "2026-01-01T00:00:00Z"

[games.minimal.scale_profile]
name = "默认"
algorithm = "Integer"
internal_width = 1280
internal_height = 720
output_width = 2560
output_height = 1440
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let game = &config.games["minimal"];
        assert!(game.save_paths.is_empty());
        assert_eq!(game.wine_prefix, None);
        assert_eq!(game.scale_profile.framerate_limit, None);
        assert!(!game.scale_profile.force_fullscreen);
        // Profiles written before scaling ratios existed: no ratio, and the
        // window is free to drive the output size.
        assert_eq!(game.scale_profile.scale_ratio, None);
        assert!(game.scale_profile.follow_window);
        assert_eq!(config.daemon.socket_path, default_socket_path());
    }
}

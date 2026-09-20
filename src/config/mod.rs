use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
mod paths;
mod profile;
mod sync;

// Only what the rest of the crate actually names. Everything else stays reachable
// through `config::profile` / `config::paths` — a re-export nobody uses is a
// warning, and a re-export list that lies about the surface is worse.
pub use paths::{
    config_path, data_dir, default_socket_path, load, load_at, log_dir, plain_secrets_path,
    resolve_socket, save, save_to, secrets_path, socket_path,
};
pub use profile::{FALLBACK_OUTPUT_HEIGHT, FALLBACK_OUTPUT_WIDTH, ScaleAlgorithm, ScaleProfile};
// `MAX_SHARPNESS` 唯一的消费者是 gamescope 的命令行(`scale::args`,unix)。
// Windows 上没有那条路,导出它只会换来一个 unused 警告。
#[cfg(unix)]
pub use profile::MAX_SHARPNESS;
pub use sync::{SyncConfig, SyncEngine};

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
    /// 自动追踪:游戏**不必**由 kotori 启动。只要它的进程出现(自己双击、Steam、
    /// 启动器拉起来的都算),kotori 就跟着记一局,游戏退出后照常上传存档。
    ///
    /// 它描述的是「怎么发现一局游戏」,**与"谁把它启动起来"无关** —— 从前的字段叫
    /// `watch_only`,语义是"kotori 永不启动这款游戏",于是"跟踪"和"启动"变成互斥的
    /// 两件事。用户 2026-09-20 指出那是错的:开着追踪照样可以从 kotori 点「启动」,
    /// 两条路发现的是同一局游戏,不该互相排斥。旧名字仍然读得进来(`alias`),默认
    /// **开**。
    #[serde(default = "default_auto_watch", alias = "watch_only")]
    pub auto_watch: bool,
    /// Launch the exe **without** gamescope: plain wine on Linux, the exe
    /// itself on Windows (where this is the only kind of launch there is).
    /// 与 [`GameConfig::auto_watch`] 无关:后者说"别人启动的也要跟",它说"kotori
    /// 启动时不套缩放"。
    #[serde(default)]
    pub direct_launch: bool,
    /// Process name to watch so save sync knows when the game is running.
    /// Useful for launcher games (where the launched process exits early) and
    /// required for watch-only games.
    #[serde(default)]
    pub process_name: Option<String>,
    /// 手写配置可以整段省略:省略时用给新游戏的那份默认档(见
    /// [`ScaleProfile::default_for`])。它曾经是必填,而少写一个必填字段的代价
    /// 不是"用默认值",是**整份配置被判解析失败**——见下面 `created_at` 的说明。
    #[serde(default = "ScaleProfile::default_for")]
    pub scale_profile: ScaleProfile,
    /// 手写配置可以省略:省略时按"读到的这一刻"记(不参与排序,纯粹是个时间戳)。
    ///
    /// 它从前也是必填,而这个项目的"解析失败"处置是**把用户的文件改名**成
    /// `config.toml.corrupt` 再拿默认值继续跑(免得坏配置把应用卡死)。两者撞在
    /// 一起就有个很难查的现象:**便携安装**(`config.toml` 放在 exe 旁边,见
    /// [`config_path`])里少写一个字段,那份配置就被改名搬走,于是**下次启动在
    /// exe 旁边找不到配置,静默切回平台默认目录** —— 用户只看到"我的配置没了"。
    /// 手写一个最小条目(只要 `name` 与 `exe_path`)因此必须能跑通。
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
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

    /// 配置与界面里用的那个拼写(`"windows"` / `"relative"` / `"absolute"`)。
    ///
    /// 与 serde 的写法**必须逐字一致**(`rename_all = "lowercase"`)—— 界面那边拿
    /// 字符串装 kind(`SAVE_PATH_KINDS` 与下拉框下标都靠它),两边对不上就是静默地把
    /// 一个位置存成另一种意思。显式写出来是因为 serde 那边的改动不会在这里报错,
    /// 所以配一条测试盯着(见文件末尾 `as_str_matches_what_serde_writes`)。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Relative => "relative",
            Self::Absolute => "absolute",
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

    /// 自动追踪时要盯的进程名:`process_name` 优先,没写就用 exe 自己的文件名
    /// (与直启那条路一致 —— wine 会把 `argv[0]` 改成它)。
    ///
    /// 两个都取不出来(配置里没有 exe、也没有进程名)时返回 `None`:没有名字就没法
    /// 认人,这一款只能等用户自己来点「启动」。
    pub fn watch_name(&self) -> Option<String> {
        self.process_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.exe_path
                    .file_name()
                    .map(|name| name.to_string_lossy().trim().to_string())
                    .filter(|name| !name.is_empty())
            })
    }
}

/// [`GameConfig::auto_watch`] 缺省值:开。
fn default_auto_watch() -> bool {
    true
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
mod tests;

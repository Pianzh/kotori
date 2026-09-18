//! 存档位置的**翻译与解析**:配置里那种可移植的写法 ↔ 这台机器上的真实目录。
//!
//! 两个方向都要有,而且必须互为逆运算 —— 「浏览…」挑回来的路径要写成档案里的写法,
//! 下次启动时再解析回同一个地方(那条来回跑的测试就在隔壁 `path_tests.rs`)。
//!
//! 与 `mod.rs` 分开的理由很实在:那边管**前缀**(哪来的、怎么找、怎么关),这边管
//! **路径怎么写**,两边只有 `home_dir` 一个交点。合在一份文件里的时候它是全仓最长
//! 的源文件(1179 行),而这两件事从来没有一起改过。

use std::path::{Path, PathBuf};

use super::{PrefixSource, home_dir, resolve_prefix};
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

// ── 反方向:用户用「浏览…」挑了一个真实路径 → 档案里那种写法 ──────────────────
//
// 上面那个方向是给"启动游戏 / 同步存档"用的,这个是给**界面**用的:选择框回来的
// 永远是本机的真实路径,而档案里存的必须是能跨系统的那三种写法之一(ADR-008)。
// 转换失败时给的是一句人能照做的话 —— 界面直接把这句话显示给用户。

/// 本机路径 → `relative` 写法(相对游戏根目录)。
///
/// 挑到游戏目录**外面**时明确拒绝:`relative` 的语义就是"相对这个游戏",塞一份绝对
/// 路径进去只会在另一台机器上变成另一个位置。宁可选不了,也不要存一个假的相对路径。
pub fn to_relative_path(game_dir: &Path, picked: &Path) -> Result<String, String> {
    let relative = picked.strip_prefix(game_dir).map_err(|_| {
        format!(
            "只能选游戏根目录({})里面的位置才能写成 relative;要么把存档放进游戏目录,\
             要么把这一行改成 absolute。",
            game_dir.display()
        )
    })?;

    // Windows 上 `Path::display` 会用 `\` 分隔,而档案里两种写法都吃(见 join_windows),
    // 统一成 `/` 是为了让同一份配置在两个系统上都好看。
    let text = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if text.is_empty() {
        return Err("选的是游戏根目录本身,不是它里面的某个位置。".to_string());
    }
    Ok(text)
}

/// 本机路径 → `windows` 写法(`%APPDATA%\Game\save`)。
///
/// 认的是**路径的形状**而不是"这是哪个 prefix",两种形状都认:
///
/// ① wine prefix:`…/drive_c/users/<用户名>/…`
/// ② 真 Windows:`<盘符>:\Users\<用户名>\…` —— 资源管理器挑回来的就是这种
///
/// 这样不需要先知道游戏用的是哪个 prefix,连游戏自带的便携 prefix 也照样认得出。
///
/// ⚠ ② 是 2026-09-18 补的:在那之前只认 ①,于是**真 Windows 上点「浏览…」永远翻译
/// 失败**;而调用点的规矩是"翻译不了就什么都不改",用户看到的就是"点了没反应"
/// (BUG-REPORT「存档位置的浏览有问题」)。
///
/// 分隔符按**字符**自己切,不走 `Path::components()`:`\` 在 Linux 上不是分隔符,而
/// 这个函数完全可能拿到一串 Windows 形状的文本 —— 切不开的话它就只能在 Windows 上
/// 才测得到,而那正是最不方便测的地方。
pub fn to_windows_token(picked: &Path) -> Result<String, String> {
    let segments: Vec<&str> = picked
        .to_str()
        .unwrap_or_default()
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();

    let Some(user_at) = user_dir_at(&segments) else {
        return Err(format!(
            "{} 不在用户目录里,写不成跨平台的令牌;请挑用户目录里的位置\
             (Windows 上是 C:\\Users\\<用户名>\\…,wine 里是 drive_c/users/<用户名>/…),\
             或者把这一行改成 absolute。",
            picked.display()
        ));
    };
    let tail = &segments[user_at..];
    if tail.is_empty() {
        return Ok("%USERPROFILE%".to_string());
    }

    // 最长匹配优先:`AppData/Local` 必须赢过任何更短的前缀(`%USERPROFILE%` 的映射
    // 是空的,它只在前面的令牌都不匹配时才兜底)。
    let mut best: Option<(&str, &str)> = None;
    for (token, mapped) in TOKENS {
        if mapped.is_empty() || tail.len() < mapped.split('/').count() {
            continue;
        }
        let matches = mapped
            .split('/')
            .zip(tail)
            .all(|(want, got)| want.eq_ignore_ascii_case(got));
        if matches && best.is_none_or(|(_, best)| mapped.len() > best.len()) {
            best = Some((token, mapped));
        }
    }

    let (token, skip) = best.unwrap_or(("%USERPROFILE%", ""));
    let rest = &tail[skip.split('/').filter(|s| !s.is_empty()).count()..];

    let mut text = token.to_string();
    for segment in rest {
        text.push('\\');
        text.push_str(segment);
    }
    Ok(text)
}

/// 用户目录(`%USERPROFILE%` 那个位置)到哪里为止 —— 返回**用户名后面那一段**的下标。
/// 两种形状各试一次,都不像就是 `None`。
fn user_dir_at(segments: &[&str]) -> Option<usize> {
    // ① wine prefix:`…/drive_c/users/<用户名>/…`
    if let Some(at) = segments.windows(3).position(|window| {
        window[0].eq_ignore_ascii_case("drive_c")
            && window[1].eq_ignore_ascii_case("users")
            && !window[2].is_empty()
    }) {
        return Some(at + 3);
    }
    // ② 真 Windows:`<盘符>:\Users\<用户名>\…`(`C:\Users\<我>\AppData\Roaming\…`)
    if segments.len() >= 3
        && is_drive_letter(segments[0])
        && segments[1].eq_ignore_ascii_case("users")
        && !segments[2].is_empty()
    {
        return Some(3);
    }
    None
}

/// `C:` 这种盘符段。字符串切过之后它自己就是一段,所以 `strip_drive_letter` 剥完
/// 什么都不剩 —— 这就是判据。
fn is_drive_letter(segment: &str) -> bool {
    strip_drive_letter(segment) == Some("")
}

/// 一个本机路径 → 档案里那种可移植的写法,**按「相对 → 令牌 → 绝对」的顺序试**
/// (用户 2026-09-18 定的顺序),连同它对应的 kind 一起给出。
///
/// 这是「浏览…」挑完位置之后该走的那条路:挑回来的路径**自己**说明它属于哪一类,
/// 而不是由用户事先在下拉框里选好 —— 选错了就翻译不成,而"翻译不成"在界面上的
/// 表现是"点了没反应"。
///
/// 三条路的取舍:
/// ① 在游戏目录里面 → [`SavePathKind::Relative`]:连盘符都不用记,换台机器照样对得上,
///    所以最优先;
/// ② 在用户目录里 → [`SavePathKind::Windows`] 令牌:跨系统,而且同一个令牌在 wine 与
///    真 Windows 上指向同一个地方(见 [`resolve_windows_path`]);
/// ③ 都不是 → [`SavePathKind::Absolute`]:只在这台机器上成立,所以**不假装**它可移植
///    —— kind 本身就把这件事说清楚了,界面上那一栏也会跟着变成「绝对路径(仅本机)」。
pub fn portable_save_path(game_dir: &Path, picked: &Path) -> (SavePathKind, String) {
    if let Ok(relative) = to_relative_path(game_dir, picked) {
        return (SavePathKind::Relative, relative);
    }
    if let Ok(token) = to_windows_token(picked) {
        return (SavePathKind::Windows, token);
    }
    (SavePathKind::Absolute, picked.display().to_string())
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

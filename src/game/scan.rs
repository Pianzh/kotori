//! 扫目录找游戏:哪些看起来是本体、哪些是补丁/工具,以及一款游戏怎么变成一条配置。
//!
//! 从 `game/mod.rs` 拆出来:那边管"库里的条目怎么增删改",这里管"一个目录里有什么"。
//! 拆的界线是**输入**:这一整块只吃一个目录路径,不碰配置(除了 `add_from_dir` 那一步)。

use std::path::{Path, PathBuf};

use super::add_games;
use crate::config::{GameConfig, ScaleProfile};

/// Scan a directory for games and display what would be added (without writing config).
///
/// A directory becomes an entry when a recognizable exe sits *directly* inside
/// it. That check runs on the scanned directory itself first — an exe at the
/// root of the directory being scanned used to be invisible to `scan <dir>`
/// even though a scan of its parent found it — and then on each subdirectory.
/// Every directory contributes at most one entry, so a self-hit and a
/// subdirectory hit can never duplicate each other.
pub fn scan(directory: &Path) -> anyhow::Result<Vec<GameConfig>> {
    if !directory.exists() {
        // An unknown path must not look like "an empty directory": the CLI has
        // to fail (exit != 0) so a typo is distinguishable from a miss.
        anyhow::bail!("目录不存在: {}", directory.display());
    }
    let mut games = Vec::new();

    // ADR-004: on a tiling compositor the output size should follow the real
    // monitor resolution instead of a hard-coded value.
    let output = crate::display::primary_resolution_or((
        crate::config::FALLBACK_OUTPUT_WIDTH,
        crate::config::FALLBACK_OUTPUT_HEIGHT,
    ));
    tracing::info!("扫描使用输出分辨率 {}x{}", output.0, output.1);

    // The scanned directory itself may be the game.
    if let Some(exe) = pick_game_exe(directory) {
        games.push(game_entry(directory, exe));
    }

    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        // Pick the best game exe for this game directory.
        if let Some(exe) = pick_game_exe(&path) {
            games.push(game_entry(&path, exe));
        }
    }

    Ok(games)
}

/// One directory with a chosen exe becomes one config entry.
pub(super) fn game_entry(dir: &Path, exe: PathBuf) -> GameConfig {
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let game_dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.to_path_buf());

    // 指纹在这一刻算（**三条添加路径共用这里**的这条，以及 `game.create` 那条）：
    // 云端要认"两台机器上哪两条档案是同一款游戏"，靠的就是它（见 `sync::fingerprint`）。
    let exe_fingerprint = crate::sync::fingerprint::of_file(&exe);

    GameConfig {
        cloud_id: None,
        exe_fingerprint,
        // 还没上传过：云端落点就是本机的游戏 id（第一次上传时按身份定下来）。
        cloud_dir: None,
        cloud_rejected: Vec::new(),
        sync_enabled: true,
        cloud_conclusion: None,
        name,
        game_dir,
        exe_path: exe,
        launch_args: Vec::new(),
        save_paths: Vec::new(),
        wine_prefix: None,
        // 新游戏默认**自动追踪**(用户 2026-09-19:「仅观测默认打开」):用户自己双击
        // 起来的那一局也照样记,不要求他非得从 kotori 点启动。
        auto_watch: true,
        direct_launch: false,
        process_name: None,
        scale_profile: ScaleProfile::default_for(),
        created_at: chrono::Utc::now(),
    }
}

/// Scan a directory and write the found games into the config file.
/// Returns the list of games that were actually added (existing entries are
/// never overwritten, so tuned profiles survive a re-scan).
///
/// 这是 `kotori add` 的**兜底**路径:没有 daemon 时它自己写配置(有 daemon 就走
/// `game.add`,见 `cli::add_cli`)。整个"读-改-写"都在配置文件的跨进程锁里 ——
/// 同时跑两条 `kotori add`,或者 daemon 恰好在这时候保存,都不会丢一笔(BUG-16)。
pub fn add_from_dir(directory: &Path) -> anyhow::Result<Vec<(String, GameConfig)>> {
    let _lock = crate::config::ConfigLock::acquire(&crate::config::config_path())?;
    let mut config = crate::config::load()?;
    let found = scan(directory)?;
    let added = add_games(&mut config, found);
    crate::config::save(&config)?;
    Ok(added)
}

/// Pick the most plausible game executable inside a directory.
///
/// Strategy:
/// 1. Drop executables that are plainly not the game ([`is_helper`]).
/// 2. Score what is left: localised builds first, then names that look like the
///    directory they live in ([`affinity`]).
/// 3. Fall back to any executable when *everything* looked like a helper — a
///    dosbox-only game really is just `dosbox.exe`.
///
/// Ties are broken by size and then by name rather than by directory order:
/// `read_dir` order is filesystem-dependent, and a library entry once pointed at
/// `Uninstaller.exe` because of it.
pub(super) fn pick_game_exe(dir: &Path) -> Option<PathBuf> {
    let dir_hint = normalize(&dir.file_name().unwrap_or_default().to_string_lossy());
    let mut exes: Vec<(String, u64, PathBuf)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "exe").unwrap_or(false) {
                let stem = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                exes.push((stem, size, p));
            }
        }
    }

    if exes.is_empty() {
        return None;
    }

    let mut candidates: Vec<(i32, u64, String, PathBuf)> = exes
        .iter()
        .filter(|(stem, _, _)| !is_helper(stem))
        .map(|(stem, size, path)| {
            let mut score = 0;
            if stem.contains("chs")
                || stem.contains("chinese")
                || stem.contains("_cn")
                || stem == "cn"
                || stem.contains("汉化")
                || stem.contains("中文")
            {
                score += 3;
            }
            if stem.contains("game") || stem.contains("启动") {
                score += 2;
            }
            if stem == "main" {
                score += 1;
            }
            score += affinity(&dir_hint, stem);
            (score, *size, stem.clone(), path.clone())
        })
        .collect();

    if candidates.is_empty() {
        // Only helpers present; fall back to any exe, name-sorted so the answer
        // does not depend on directory order.
        exes.sort_by(|a, b| a.0.cmp(&b.0));
        return exes.first().map(|(_, _, p)| p.clone());
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    candidates.into_iter().next().map(|(_, _, _, p)| p)
}

/// Is this executable something other than the game?
///
/// Installers, config tools and update helpers. Matched as *prefixes* as well as
/// whole names, because `Uninstaller.exe`, `unins000.exe` and `Setup_x.exe` all
/// mean the same thing — the list only knowing the bare word "uninstall" is how a
/// library entry came to point at an uninstaller.
pub(super) fn is_helper(stem: &str) -> bool {
    const EXACT: &[&str] = &[
        "uninstall",
        "uninst",
        "setup",
        "settings",
        "startuptool",
        "authtool",
        "autoupdate",
        "filechk",
        "resetconfig",
        "opentsalpha",
        "config",
        "envcheck",
        "bootmenu",
        "sigluscounter",
        "dosbox",
        "launcher",
        "注册表恢复",
        "注册表修复",
        "安装",
        "卸载",
        "工具",
        "设置",
    ];
    const PREFIXES: &[&str] = &[
        "unins",
        "setup",
        "install",
        "config",
        "filechk",
        "bootmenu",
        "autoupdate",
        "startuptool",
        "opentsalpha",
        "sigluscounter",
        "dosbox",
    ];
    const CONTAINS: &[&str] = &[
        "unitycrash",
        "game_manager",
        "修改工具",
        "修复器",
        "manager",
        "opensavefolder",
        "savefolder",
        "readme",
        // A localisation *patch installer* is not the game, even though its name
        // contains the same hint that makes a localised build attractive: a real
        // entry pointed at `灰色的果实_汉化补丁.exe` while `Grisaia.exe` sat next
        // to it.
        "补丁",
        "patch",
        "crack",
        "免cd",
        "破解",
        "激活",
    ];
    EXACT.contains(&stem)
        || PREFIXES.iter().any(|prefix| stem.starts_with(prefix))
        || CONTAINS.iter().any(|needle| stem.contains(needle))
}

/// Lower-case a name and drop everything that is not a letter or a digit, so
/// `Rance3` and `Rance03` can be compared at all.
pub(super) fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// How much an executable's name looks like the directory it lives in.
///
/// `Rance3/Rance03.exe` and `SiglusEngineCHS/SiglusEngineCHS.exe` are the normal
/// case; leftovers like `OpenSaveFolder.exe` share nothing with the directory.
/// An exact match scores highest, a shared prefix less, and it can never outvote
/// the localisation hints above — it only decides between plausible candidates.
pub(super) fn affinity(dir_hint: &str, stem: &str) -> i32 {
    let stem = normalize(stem);
    if stem.chars().count() < 4 {
        return 0;
    }
    if stem == *dir_hint {
        return 4;
    }
    let shared = dir_hint
        .chars()
        .zip(stem.chars())
        .take_while(|(a, b)| a == b)
        .count();
    if shared >= 4 {
        (shared as i32).min(3)
    } else {
        0
    }
}

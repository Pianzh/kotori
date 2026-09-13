use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{GameConfig, ScaleProfile};

/// Launch a game with its bound scaling profile.
///
/// The daemon owns all game processes, so this goes through the IPC socket
/// (booting the daemon first if needed) rather than spawning gamescope in the
/// CLI process.
///
/// If `wait` is true, blocks until the game (gamescope session) exits.
pub async fn launch(game_id: &str, wait: bool) -> anyhow::Result<String> {
    let socket = crate::config::socket_path();
    crate::daemon::ensure_running(&socket)?;

    let response = crate::rpc::call(
        &socket,
        "game.launch",
        Some(crate::rpc::params([(
            "id",
            Value::String(game_id.to_string()),
        )])),
    )
    .await
    .map_err(|e| anyhow::anyhow!("启动失败: {e}"))?;

    let session_id = response
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("守护进程未返回 session_id"))?
        .to_string();

    tracing::info!("game launched: session_id={session_id}");

    if wait {
        crate::rpc::call(
            &socket,
            "game.wait",
            Some(crate::rpc::params([(
                "session_id",
                Value::String(session_id.clone()),
            )])),
        )
        .await
        .map_err(|e| anyhow::anyhow!("等待游戏退出失败: {e}"))?;
        tracing::info!("game {game_id} exited");
    }

    Ok(session_id)
}

/// List configured games.
pub fn list() -> anyhow::Result<()> {
    let config = crate::config::load()?;

    if config.games.is_empty() {
        println!("No games configured. Use `kotori scan <dir>` to scan a directory.");
        return Ok(());
    }

    println!("Configured games:");
    for (id, game) in &config.games {
        println!("  {} - {}", id, game.name);
        println!("    exe:     {}", game.exe_path.display());
        let scale = &game.scale_profile;
        // 留空＝自动(启动时按屏幕算),所以这里要说"自动"而不是印两个 0。
        let output = match scale.explicit_output_size() {
            Some((width, height)) => format!("{width}x{height}"),
            None => "自动（按屏幕）".to_string(),
        };
        println!(
            "    scale:   {} ({}x{} -> {})",
            scale.name, scale.internal_width, scale.internal_height, output,
        );
        println!(
            "    saved:   {}",
            if game.save_paths.is_empty() {
                "none".to_string()
            } else {
                game.save_paths
                    .iter()
                    .map(|p| p.describe())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }

    Ok(())
}

/// Scan a directory for games and display what would be added (without writing config).
pub fn scan(directory: &Path) -> anyhow::Result<Vec<GameConfig>> {
    let mut games = Vec::new();

    if !directory.exists() {
        return Ok(games);
    }

    // ADR-004: on a tiling compositor the output size should follow the real
    // monitor resolution instead of a hard-coded value.
    let output = crate::display::primary_resolution_or((
        crate::config::FALLBACK_OUTPUT_WIDTH,
        crate::config::FALLBACK_OUTPUT_HEIGHT,
    ));
    tracing::info!("扫描使用输出分辨率 {}x{}", output.0, output.1);

    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        // Pick the best game exe for this game directory.
        if let Some(exe) = pick_game_exe(&path) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let game_dir = exe
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| path.clone());

            games.push(GameConfig {
                name,
                game_dir,
                exe_path: exe,
                launch_args: Vec::new(),
                save_paths: Vec::new(),
                wine_prefix: None,
                watch_only: false,
                process_name: None,
                scale_profile: ScaleProfile::default_for(),
                created_at: chrono::Utc::now(),
            });
        }
    }

    Ok(games)
}

/// Scan a directory and write the found games into the config file.
/// Returns the list of games that were actually added (existing entries are
/// never overwritten, so tuned profiles survive a re-scan).
pub fn add_from_dir(directory: &Path) -> anyhow::Result<Vec<(String, GameConfig)>> {
    let mut config = crate::config::load()?;
    let found = scan(directory)?;
    let added = add_games(&mut config, found);
    crate::config::save(&config)?;
    Ok(added)
}

/// Insert scanned games into `config`, skipping ids that already exist so that
/// re-scanning never overwrites a tuned profile.
///
/// Pure: the caller owns persistence (the daemon persists atomically, the CLI
/// writes the file directly). Returns what was actually added.
pub fn add_games(
    config: &mut crate::config::Config,
    found: Vec<GameConfig>,
) -> Vec<(String, GameConfig)> {
    let mut added = Vec::new();
    for game in found {
        let id = generate_game_id(&game.name);
        if let std::collections::hash_map::Entry::Vacant(slot) = config.games.entry(id.clone()) {
            slot.insert(game.clone());
            added.push((id, game));
        }
    }
    added
}

/// Remove a game from the config. Returns false when the id is unknown.
pub fn remove_game(config: &mut crate::config::Config, id: &str) -> bool {
    config.games.remove(id).is_some()
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
fn pick_game_exe(dir: &Path) -> Option<PathBuf> {
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
fn is_helper(stem: &str) -> bool {
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
fn normalize(name: &str) -> String {
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
fn affinity(dir_hint: &str, stem: &str) -> i32 {
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

/// Generate a stable id from a directory name.
pub fn generate_game_id(dir_name: &str) -> String {
    dir_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-cleaning scratch directory under the system temp dir.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-game-{}-{}-{}",
                tag,
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn with(&self, files: &[&str]) -> &Self {
            for file in files {
                std::fs::write(self.0.join(file), b"").unwrap();
            }
            self
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn file_name(picked: &Path) -> String {
        picked.file_name().unwrap().to_string_lossy().to_string()
    }

    #[test]
    fn prefers_the_chinese_localised_executable() {
        let dir = TempDir::new("chs");
        dir.with(&["game.exe", "game.chs.exe", "readme.txt"]);
        let picked = pick_game_exe(&dir.path()).unwrap();
        assert_eq!(file_name(&picked), "game.chs.exe");
    }

    #[test]
    fn skips_installers_and_tools() {
        let dir = TempDir::new("helpers");
        dir.with(&[
            "setup.exe",
            "uninstall.exe",
            "sigluscounter.exe",
            "注册表恢复.exe",
            "ADVcore.exe",
        ]);
        let picked = pick_game_exe(&dir.path()).unwrap();
        assert_eq!(file_name(&picked), "ADVcore.exe");
    }

    #[test]
    fn falls_back_to_an_exe_when_only_helpers_exist() {
        // Documents the current last-resort behaviour (e.g. dosbox.exe games).
        let dir = TempDir::new("only-helpers");
        dir.with(&["dosbox.exe"]);
        let picked = pick_game_exe(&dir.path()).unwrap();
        assert_eq!(file_name(&picked), "dosbox.exe");
    }

    #[test]
    fn no_executables_means_no_candidate() {
        let dir = TempDir::new("no-exe");
        dir.with(&["readme.txt", "data.pak"]);
        assert!(pick_game_exe(&dir.path()).is_none());
    }

    #[test]
    fn game_ids_are_stable_and_filesystem_safe() {
        assert_eq!(generate_game_id("My Game v0.99"), "my-game-v0-99");
        assert_eq!(generate_game_id("测试游戏(正式版)"), "测试游戏-正式版");
        assert_eq!(generate_game_id("My-Game"), "my-game");
        assert_eq!(generate_game_id("  "), "");
        // Same directory name must always produce the same id.
        assert_eq!(generate_game_id("SomeGame"), generate_game_id("SomeGame"));
    }

    #[test]
    fn scan_skips_non_directories_and_dirs_without_exes() {
        let root = TempDir::new("scan-root");
        std::fs::create_dir_all(root.path().join("GameA")).unwrap();
        std::fs::write(root.path().join("GameA").join("game.chs.exe"), b"").unwrap();
        std::fs::create_dir_all(root.path().join("EmptyGame")).unwrap();
        std::fs::write(root.path().join("loose.exe"), b"").unwrap();

        let found = scan(&root.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "GameA");
        assert_eq!(file_name(&found[0].exe_path), "game.chs.exe");
        assert_eq!(found[0].scale_profile.internal_width, 1280);
        // 扫描**不再**把某台机器的分辨率写进档案(2026-09-13):窗口尺寸留空,
        // 启动时按游戏实际落在的那块屏算(见 `ScaleProfile::output_size_for`)。
        // 这样换显示器、换机器都不用重新扫描。
        assert_eq!(found[0].scale_profile.output_width, None);
        assert_eq!(found[0].scale_profile.output_height, None);
        assert_eq!(found[0].scale_profile.explicit_output_size(), None);
    }

    #[test]
    fn scan_of_missing_directory_is_empty_not_an_error() {
        let missing = std::env::temp_dir().join("kotori-does-not-exist-xyz");
        assert!(scan(&missing).unwrap().is_empty());
    }

    #[test]
    fn an_uninstaller_is_not_the_game_even_when_it_is_called_uninstaller() {
        // Measured on the real library: `rance3/` had picked `Uninstaller.exe`,
        // because the helper list only knew the bare word "uninstall" (so
        // "uninstaller" was not filtered) and ties fell to directory order.
        let dir = TempDir::new("rance3");
        dir.with(&[
            "OpenSaveFolder.exe",
            "Rance03.exe",
            "ResetConfig.exe",
            "Uninstaller.exe",
        ]);
        let picked = pick_game_exe(&dir.path()).unwrap();
        assert_eq!(file_name(&picked), "Rance03.exe");
    }

    #[test]
    fn the_executable_named_like_its_directory_beats_a_lookalike() {
        // A localised build is usually the one that carries the directory's name.
        let dir = TempDir::new("affinity");
        let game = dir.path().join("SiglusEngineCHS");
        std::fs::create_dir_all(&game).unwrap();
        for file in [
            "SiglusEngine.exe",
            "SiglusEngineCHS.exe",
            "SiglusCounter.exe",
        ] {
            std::fs::write(game.join(file), b"").unwrap();
        }
        let picked = pick_game_exe(&game).unwrap();
        assert_eq!(file_name(&picked), "SiglusEngineCHS.exe");
    }

    #[test]
    fn helpers_are_recognised_by_prefix_too() {
        for stem in [
            "uninstaller",
            "unins000",
            "setup_x",
            "installer",
            "configtool",
            "autoupdatecheck",
        ] {
            assert!(is_helper(stem), "{stem} 应该被当作工具");
        }
        for stem in ["rance03", "advcore", "siglusenginechs", "game"] {
            assert!(!is_helper(stem), "{stem} 是游戏本体，不该被过滤");
        }
    }
}

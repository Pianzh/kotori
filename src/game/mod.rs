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
        println!(
            "    scale:   {} ({}x{} -> {}x{})",
            game.scale_profile.name,
            game.scale_profile.internal_width,
            game.scale_profile.internal_height,
            game.scale_profile.output_width,
            game.scale_profile.output_height,
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
                scale_profile: ScaleProfile::default_for(output),
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
/// 1. Prefer executables whose basename hints it's the game
///    (e.g. contains "chs", "chinese", "cn", "game", "启动").
/// 2. Skip obvious helper/utility executables.
/// 3. Fall back to the first remaining .exe.
fn pick_game_exe(dir: &Path) -> Option<PathBuf> {
    let mut exes: Vec<(String, PathBuf)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "exe").unwrap_or(false) {
                let stem = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                exes.push((stem, p));
            }
        }
    }

    if exes.is_empty() {
        return None;
    }

    // Score each candidate.
    let candidates: Vec<(i32, PathBuf)> = exes
        .iter()
        .filter_map(|(stem, path)| {
            let is_helper = helper_names().iter().any(|h| stem == *h)
                || stem.contains("unitycrash")
                || stem.contains("game_manager")
                || stem.contains("dosbox")
                || stem.contains("修改工具")
                || stem.contains("修复器")
                || stem.contains("manager");
            if is_helper {
                return None;
            }
            let mut score = 0;
            if stem.contains("chs")
                || stem.contains("chinese")
                || stem.contains("_cn")
                || stem == "cn"
                || stem.contains("汉化")
            {
                score += 3;
            }
            if stem.contains("game") || stem.contains("启动") {
                score += 2;
            }
            if stem == "main" {
                score += 1;
            }
            Some((score, path.clone()))
        })
        .collect();

    if candidates.is_empty() {
        // Only helpers present; fall back to any exe.
        return exes.first().map(|(_, p)| p.clone());
    }

    candidates
        .into_iter()
        .max_by_key(|(score, _)| *score)
        .map(|(_, p)| p)
}

const fn helper_names() -> &'static [&'static str] {
    &[
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
    ]
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
        // The output resolution comes from display discovery, never a constant.
        let expected = crate::display::primary_resolution_or((
            crate::config::FALLBACK_OUTPUT_WIDTH,
            crate::config::FALLBACK_OUTPUT_HEIGHT,
        ));
        assert_eq!(
            (
                found[0].scale_profile.output_width,
                found[0].scale_profile.output_height
            ),
            expected
        );
    }

    #[test]
    fn scan_of_missing_directory_is_empty_not_an_error() {
        let missing = std::env::temp_dir().join("kotori-does-not-exist-xyz");
        assert!(scan(&missing).unwrap().is_empty());
    }
}

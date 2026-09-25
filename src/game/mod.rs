use std::path::Path;

use serde_json::Value;

use crate::config::GameConfig;

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
mod scan;

// 扫描那一块整体住在 `scan.rs`；这几个名字原来就在 `crate::game` 下（CLI 与 daemon
// 都在用），所以在这里原样转发。
pub use scan::{add_from_dir, scan};

pub fn list() -> anyhow::Result<()> {
    let config = crate::config::load()?;

    if config.games.is_empty() {
        println!("No games configured. Use `kotori scan <dir>` to scan a directory.");
        return Ok(());
    }

    println!("Configured games:");
    // 稳定的顺序：`config.games` 是 HashMap，直接遍历的话同一份配置每次印出来的
    // 次序都可能不同，脚本与人工对照都失去依据（BUG-37）。
    let mut games: Vec<_> = config.games.iter().collect();
    games.sort_by(|a, b| a.0.cmp(b.0));
    for (id, game) in games {
        println!("  {} - {}", id, game.name);
        match game.resolved_exe() {
            Ok(exe) => println!("    exe:     {}", exe.display()),
            Err(error) => println!("    exe:     {error}"),
        }
        // 这两个开关从前只活在配置里，`list` 看不见（BUG-14）：CLI 用户没法确认
        // 这一条到底是自动追踪、只观测会话还是直接启动。
        println!("    watch:   {}", game.auto_watch);
        println!("    direct:  {}", game.direct_launch);
        let scale = &game.scale_profile;
        // 两处留空都要说"自动",而不是印两个 0:游戏分辨率留空＝由 gamescope
        // 定(它自己的默认值),输出尺寸留空＝启动时按屏幕算。
        let internal = match scale.explicit_internal_size() {
            Some((width, height)) => format!("{width}x{height}"),
            None => "自动（gamescope 默认）".to_string(),
        };
        let output = match scale.explicit_output_size() {
            Some((width, height)) => format!("{width}x{height}"),
            None => "自动（按屏幕）".to_string(),
        };
        println!("    scale:   {} ({} -> {})", scale.name, internal, output,);
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
/// Insert scanned games into `config`. Dedup runs on the **executable**: a
/// re-scan of the same directory finds the same exe and is skipped (so a tuned
/// profile survives), while different directories whose names normalize to the
/// same id (`a&b` / `a—b` / `a-b` all become `a-b`) are **different games** —
/// they get a suffixed id and are all added, instead of being silently
/// swallowed by an id collision (BUG-6, measured 2026-09-19).
///
/// Pure: the caller owns persistence (the daemon persists atomically, the CLI
/// writes the file directly). Returns what was actually added.
pub fn add_games(
    config: &mut crate::config::Config,
    found: Vec<GameConfig>,
) -> Vec<(String, GameConfig)> {
    let mut added = Vec::new();
    for game in found {
        if config.games.values().any(|existing| {
            existing
                .resolved_exe()
                .is_ok_and(|exe| exe == game.exe_path)
        }) {
            continue;
        }
        let id = generate_unique_game_id(config, &game.name);
        config.games.insert(id.clone(), game.clone());
        added.push((id, game));
    }
    added
}

/// Remove a game from the config. Returns false when the id is unknown.
pub fn remove_game(config: &mut crate::config::Config, id: &str) -> bool {
    config.games.remove(id).is_some()
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

/// [`generate_game_id`], but collisions get a numeric suffix (`name-2`, …)
/// instead of being an error: different directories can normalize to the same id
/// (see [`add_games`]), and a UUID would fix the collision by making the id
/// unreadable — it also shows up in the cloud layout.
pub fn generate_unique_game_id(config: &crate::config::Config, name: &str) -> String {
    let base = generate_game_id(name);
    let mut candidate = base.clone();
    let mut n = 2;
    while config.games.contains_key(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

/// 这个 exe 是不是已经属于某一条档案了?是的话返回**那一条的名字**。
///
/// 与 [`duplicate_exe_warning`] 的分工是**用途**:那条是"警告但放行"(CLI 的
/// `add` 走它,旧规矩没变),这条给 `game.create` 的硬拒绝 —— 用户 2026-09-20 认定
/// 同一个 exe 建两条档案会给云同步留下说不清的坑(版本历史按档案分开存,观测同一个
/// 进程时分不清谁在跑),"能保证不出问题"之前不如挡住。**只用在创建那条路上**:
/// `game.update` 与 `scan` 暂时照旧(用户当天说这条先搁置)。
pub fn exe_owner(
    config: &crate::config::Config,
    exe_path: &Path,
    exclude_id: Option<&str>,
) -> Option<String> {
    config
        .games
        .iter()
        .find(|(id, game)| {
            Some(id.as_str()) != exclude_id
                && game
                    .resolved_exe()
                    .is_ok_and(|exe| crate::util::same_file(&exe, exe_path))
        })
        .map(|(_, game)| game.name.clone())
}

/// 「这个 exe 已经在库里了」的一句话。
///
/// 规矩是**一个 exe 只许有一条档案**：直接启动与自动追踪本来就是同一条档案上的两个
/// 开关（用户 2026-09-21 原话："它本来就只是一个选项，应该是同一个档案的"），
/// `game.create` 已经硬拒绝，`game.update` 与扫描那条路的收口还在计划里。
///
/// 今天只剩 CLI 的 `add` 还在用它，而那条路的行为是**跳过**已入库的 exe，所以这句提示
/// 实际只在"同一个文件的两种写法"时才可能出现。
///
/// Paths are compared canonicalized (resolving symlinks; both sides fall back
/// to the literal path when that fails) so the same file under a different
/// spelling still counts. `exclude_id` lets a caller that already inserted the
/// new entry skip itself.
pub fn duplicate_exe_warning(
    config: &crate::config::Config,
    exe_path: &Path,
    exclude_id: Option<&str>,
) -> Option<String> {
    let wanted = std::fs::canonicalize(exe_path).unwrap_or_else(|_| exe_path.to_path_buf());
    let same: Vec<&str> = config
        .games
        .iter()
        .filter(|(id, game)| {
            Some(id.as_str()) != exclude_id
                && game
                    .resolved_exe()
                    .is_ok_and(|exe| std::fs::canonicalize(&exe).unwrap_or(exe) == wanted)
        })
        .map(|(_, game)| game.name.as_str())
        .collect();
    if same.is_empty() {
        return None;
    }
    Some(format!(
        "可执行文件已被这些档案使用：{}。同一个 exe 现在只许有一条档案，多出来的那条\
         要么是同一个文件的另一种写法（相对路径、符号链接），要么是旧配置留下来的 ——\
         请合并或删掉多余的那条。",
        same.join("、")
    ))
}

#[cfg(test)]
mod tests;

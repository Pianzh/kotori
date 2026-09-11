mod cli;
mod config;
mod daemon;
mod display;
mod game;
mod process;
mod rpc;
mod scale;
mod secrets;
mod sync;
mod ui;
mod util;
mod wine;

use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

fn main() -> anyhow::Result<()> {
    // Initialize logging
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = cli::Cli::parse();

    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        cli::Command::Daemon => {
            tracing::info!("Starting daemon mode");
            rt.block_on(async { daemon::run().await })?;
        }
        cli::Command::Ui => {
            tracing::info!("Starting UI mode");
            ui::run()?;
        }
        cli::Command::Launch { game_id } => {
            tracing::info!("Launching game: {}", game_id);
            // CLI front: wait for the game to exit (foreground).
            let session_id = rt.block_on(async { game::launch(&game_id, true).await })?;
            tracing::info!("session finished: {}", session_id);
        }
        cli::Command::List => {
            game::list()?;
        }
        cli::Command::Status => {
            let socket = config::socket_path();
            let status = rt.block_on(async { rpc::call(&socket, "daemon.status", None).await });
            match status {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
                Err(e) => {
                    println!("守护进程未响应（{}）: {e}", socket.display());
                    std::process::exit(1);
                }
            }
        }
        cli::Command::Shutdown => {
            let socket = config::socket_path();
            match rt.block_on(async { rpc::call(&socket, "daemon.shutdown", None).await }) {
                Ok(_) => println!("守护进程已停止（正在运行的游戏不受影响）"),
                Err(e) => {
                    println!("守护进程未响应（{}）: {e}", socket.display());
                    std::process::exit(1);
                }
            }
        }
        cli::Command::Scan { directory } => {
            let games = game::scan(&directory)?;
            if games.is_empty() {
                println!("No games found in {}", directory.display());
            } else {
                println!("Found {} game(s) in {}:", games.len(), directory.display());
                for g in &games {
                    let id = game::generate_game_id(&g.name);
                    println!("  [{}] {} -> {}", id, g.name, g.exe_path.display());
                }
            }
        }
        cli::Command::Add { directory } => {
            let added = game::add_from_dir(&directory)?;
            if added.is_empty() {
                println!("No new games added from {}", directory.display());
            } else {
                println!("Added {} game(s):", added.len());
                for (id, g) in &added {
                    println!("  [{}] {} -> {}", id, g.name, g.exe_path.display());
                }
            }
        }
        cli::Command::Sync { action } => {
            sync_cli(&rt, action)?;
        }
    }

    Ok(())
}

/// `kotori sync …`: everything goes through the daemon, like the GUI does.
///
/// The daemon owns the keyring handle and the config, so a second process
/// reading the config directly would be a second writer.
fn sync_cli(rt: &tokio::runtime::Runtime, action: cli::SyncCommand) -> anyhow::Result<()> {
    use cli::SyncCommand;

    let socket = config::socket_path();
    daemon::ensure_running(&socket)?;

    let (method, params) = match action {
        SyncCommand::Status => ("sync.status", rpc::params([])),
        SyncCommand::Test => ("sync.test", rpc::params([])),
        SyncCommand::Now { game_id } => (
            "sync.now",
            rpc::params(game_id.map(|id| ("id", serde_json::Value::String(id)))),
        ),
        SyncCommand::Versions { game_id } => (
            "sync.versions",
            rpc::params([("id", serde_json::Value::String(game_id))]),
        ),
        SyncCommand::Restore { game_id, version } => {
            let mut params = rpc::params([("id", serde_json::Value::String(game_id))]);
            if let Some(version) = version {
                params.insert("version".into(), serde_json::Value::String(version));
            }
            ("sync.restore", params)
        }
    };

    let result = rt.block_on(async { rpc::call(&socket, method, Some(params)).await });
    match result {
        Ok(value) => {
            print_sync_result(method, &value);
            Ok(())
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Sync output is meant to be read by a human, not piped into `jq` — so it is
/// summarised rather than dumped. `--json` can come later if it is ever needed.
fn print_sync_result(method: &str, value: &serde_json::Value) {
    match method {
        "sync.status" => {
            let on = |v: &serde_json::Value| v.as_bool().unwrap_or(false);
            println!(
                "云同步: {}",
                if on(&value["enabled"]) {
                    "已启用"
                } else {
                    "关闭"
                }
            );
            println!("远端: {}", value["remote"].as_str().unwrap_or("-"));
            println!(
                "rclone: {}",
                value["rclone"]
                    .as_str()
                    .unwrap_or("未安装（Arch: pacman -S rclone）")
            );
            println!(
                "密钥环: {}",
                value["keyring"]["backend"].as_str().unwrap_or("-")
            );
            if let Some(problem) = value["problem"].as_str() {
                println!("待解决: {problem}");
            }
            println!(
                "取回密码: {}",
                value["password_hint"].as_str().unwrap_or("-")
            );
            if let Some(games) = value["games"].as_array() {
                println!("游戏（{} 个）:", games.len());
                for game in games {
                    let last = &game["last"];
                    let when = if last.is_null() {
                        "还没同步过".to_string()
                    } else {
                        format!(
                            "{} {} {}",
                            last["at"].as_str().unwrap_or("-"),
                            last["action"].as_str().unwrap_or(""),
                            last["detail"].as_str().unwrap_or("")
                        )
                    };
                    println!(
                        "  [{}] {} — {} 个存档位置，{when}",
                        game["id"].as_str().unwrap_or("?"),
                        game["name"].as_str().unwrap_or("?"),
                        game["locations"].as_u64().unwrap_or(0)
                    );
                    if let Some(problem) = game["location_problem"].as_str() {
                        println!("      ⚠ {problem}");
                    }
                }
            }
        }
        "sync.test" => println!(
            "连接正常: {}",
            value["remote"].as_str().unwrap_or("(未知远端)")
        ),
        "sync.versions" => {
            let versions = value["versions"].as_array().cloned().unwrap_or_default();
            if versions.is_empty() {
                println!("云端还没有这个游戏的快照");
            } else {
                println!("快照（最旧在前）:");
                for version in versions {
                    println!("  {}", version.as_str().unwrap_or("-"));
                }
            }
        }
        "sync.now" | "sync.restore" => {
            // A single game comes back under `game`; a bulk run under `games`.
            let games: Vec<&serde_json::Value> = match (
                value["games"].as_array(),
                value.get("game").filter(|g| !g.is_null()),
            ) {
                (Some(games), _) => games.iter().collect(),
                (None, Some(game)) => vec![game],
                _ => Vec::new(),
            };
            let mut failed = false;
            for game in games {
                let name = game["name"].as_str().unwrap_or("?");
                let id = game["game_id"].as_str().unwrap_or("?");
                if let Some(error) = game["error"].as_str() {
                    failed = true;
                    println!("✗ [{id}] {name}: {error}");
                } else {
                    println!("✓ [{id}] {name}");
                }
                for location in game["locations"].as_array().into_iter().flatten() {
                    println!(
                        "    {} {} — {}",
                        location["action"].as_str().unwrap_or("-"),
                        location["configured"].as_str().unwrap_or("-"),
                        location["detail"].as_str().unwrap_or("")
                    );
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        _ => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    }
}

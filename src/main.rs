mod cli;
mod config;
mod daemon;
mod display;
mod game;
mod rpc;
mod scale;
mod ui;
mod util;

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
    }

    Ok(())
}

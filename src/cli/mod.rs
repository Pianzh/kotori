use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kotori")]
#[command(about = "Galgame manager with scaling and cloud sync")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start daemon mode
    Daemon,
    /// Start UI mode
    Ui,
    /// Launch a game
    Launch {
        /// Game ID to launch
        game_id: String,
    },
    /// List all configured games
    List,
    /// Show daemon status and running sessions
    Status,
    /// Ask the running daemon to shut down
    Shutdown,
    /// Scan a directory for games and print what's found
    Scan {
        /// Directory to scan (e.g. /run/media/.../BTL)
        directory: PathBuf,
    },
    /// Scan a directory and add found games to the config
    Add {
        /// Directory to scan
        directory: PathBuf,
    },
}

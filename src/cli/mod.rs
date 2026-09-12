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
    /// Cloud save sync (B2 over rclone)
    Sync {
        #[command(subcommand)]
        action: SyncCommand,
    },
    /// Runtime scaling of a running game (a hotkey changes gamescope's scaler)
    Scale {
        #[command(subcommand)]
        action: ScaleCommand,
    },
}

#[derive(Subcommand)]
pub enum ScaleCommand {
    /// Show running sessions and whether the scaling hotkeys are usable
    Status,
    /// Ask the portal for the scaling hotkeys (approve the dialog it opens)
    Hotkeys {
        /// Seconds to wait for approval; 0 asks and returns immediately
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Toggle FSR upscaling
    Fsr {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Toggle nearest-neighbour (integer) upscaling
    Integer {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Nudge sharpness: positive is sharper, negative is softer
    Sharpness {
        /// Steps, e.g. 1 or -1
        delta: i32,
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Show what is configured, what is missing, and when each game last synced
    Status,
    /// Upload saves now (one game, or every game that has save locations)
    Now {
        /// Game ID; omit to sync everything
        game_id: Option<String>,
    },
    /// List the snapshots the cloud holds for a game
    Versions {
        /// Game ID
        game_id: String,
    },
    /// Put a game's saves back (newest state, or one snapshot)
    Restore {
        /// Game ID
        game_id: String,
        /// Snapshot to roll back to, e.g. 20260911T101500Z
        #[arg(long)]
        version: Option<String>,
    },
    /// Check the credentials and the bucket
    Test,
    /// Unlock the master-password credential file (prompts, no echo)
    Unlock,
    /// Store the credentials in a master-password file (prompts, no echo)
    MasterPassword,
    /// Forget the key until the master password is entered again
    Lock,
}

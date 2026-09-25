use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod add_cli;
mod scale_cli;
mod sync_cli;
pub(crate) use add_cli::add_cli;
pub(crate) use scale_cli::scale_cli;
pub(crate) use sync_cli::sync_cli;

#[derive(Parser)]
#[command(name = "kotori")]
#[command(about = "Galgame manager with scaling and cloud sync")]
pub struct Cli {
    /// 不给子命令时直接启动 UI —— 双击可执行文件 / 点桌面图标的默认行为。
    /// 要看帮助仍然可以 `kotori --help`。
    #[command(subcommand)]
    pub command: Option<Command>,
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
    /// Re-read config.toml (for when you edited it by hand)
    Reload,
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
    /// Runtime scaling of a running game (what the CLI and the GUI ask for)
    Scale {
        #[command(subcommand)]
        action: ScaleCommand,
    },
}

#[derive(Subcommand)]
pub enum ScaleCommand {
    /// Show running sessions, and what gamescope is scaling them with right now
    Status,
    /// Toggle FSR upscaling
    Fsr {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Toggle NIS upscaling
    Nis {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Toggle nearest-neighbour (integer) upscaling
    Integer {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Switch back to plain bilinear filtering
    Linear {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Nudge sharpness: positive is sharper, negative is softer
    Sharpness {
        /// Steps, e.g. 1 or -1
        #[arg(allow_negative_numbers = true)]
        delta: i32,
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Scale to the profile's ratio, or back to 1:1
    Toggle {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Step the upscale ratio up (the game window grows)
    Up {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Step the upscale ratio down (the game window shrinks)
    Down {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Back to 1:1 — the game at its own resolution, no upscaling
    Reset {
        /// Session to act on; only needed with more than one game running
        session_id: Option<String>,
    },
    /// Toggle the game window's fullscreen state (KDE only)
    Fullscreen {
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
    /// List the games the cloud holds (not only the ones this machine knows)
    Cloud,
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

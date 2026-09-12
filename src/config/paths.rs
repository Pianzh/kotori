//! Where kotori keeps things, and how the config file is read and written.
//!
//! Every path is injectable through the environment (ADR-006), which is what lets
//! the integration tests run a real daemon against a temporary config.

use std::path::{Path, PathBuf};

use super::Config;

pub fn default_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("kotori.sock")
}

/// Path of the config file. `KOTORI_CONFIG` overrides it (used by tests and
/// portable installs).
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kotori")
        .join("config.toml")
}

/// Path of the master-password credential file.
///
/// It lives next to the config rather than in the data directory because it is
/// configuration-shaped: one per machine, not per dataset. `KOTORI_SECRETS_FILE`
/// overrides it (tests rely on that).
pub fn secrets_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_SECRETS_FILE") {
        return PathBuf::from(p);
    }
    config_path()
        .parent()
        .map(|dir| dir.join("secrets.json"))
        .unwrap_or_else(|| PathBuf::from("secrets.json"))
}

/// Data directory (`~/.local/share/kotori`). `KOTORI_DATA_DIR` overrides it.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kotori")
}

/// Directory holding daemon logs (`<data_dir>/logs`).
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Resolve the daemon socket for a given config.
/// `KOTORI_SOCKET` overrides the config value.
pub fn resolve_socket(config: &Config) -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_SOCKET") {
        return PathBuf::from(p);
    }
    config.daemon.socket_path.clone()
}

/// Resolve the daemon socket, loading the config (falling back to defaults).
pub fn socket_path() -> PathBuf {
    resolve_socket(&load().unwrap_or_default())
}

/// Load the user config.
///
/// A missing file yields defaults. A *corrupt* file is moved aside to
/// `<path>.corrupt` and defaults are returned, so a broken config never bricks
/// the app while the user's data stays recoverable (GOALS §6.2).
pub fn load() -> anyhow::Result<Config> {
    load_at(&config_path())
}

/// [`load`] against an explicit path.
///
/// The daemon remembers the file it was started with instead of re-resolving it
/// on every write, so a config that was loaded from one path can never be saved
/// over another.
pub fn load_at(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    match load_from(path) {
        Ok(config) => Ok(config),
        Err(err) => {
            let backup = path.with_extension("toml.corrupt");
            let moved = std::fs::rename(path, &backup).is_ok();
            if moved {
                tracing::error!(
                    "配置解析失败，已备份到 {}，本次使用默认配置: {err}",
                    backup.display()
                );
            } else {
                tracing::error!("配置解析失败，本次使用默认配置: {err}");
            }
            Ok(Config::default())
        }
    }
}

/// Load and parse a config from an explicit path (strict: no fallback).
pub fn load_from(path: &Path) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let mut config: Config = toml::from_str(&content)?;
    config.normalize();
    Ok(config)
}

/// Save the config to the default path.
pub fn save(config: &Config) -> anyhow::Result<()> {
    save_to(&config_path(), config)
}

/// Save the config to an explicit path, creating parent directories.
pub fn save_to(path: &Path, config: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(path, content)?;
    Ok(())
}

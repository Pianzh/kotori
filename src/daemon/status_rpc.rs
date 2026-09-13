//! The requests about the daemon's own state rather than about a game: its
//! status, the wine prefix it would use, and reloading the config from disk.

use serde_json::{Value, json};

use super::*;

impl Daemon {
    pub(super) async fn rpc_status(&self) -> Result<Value, String> {
        let games = self.config.read().await.games.len();
        let sessions = self.engine.list_sessions().await;
        Ok(json!({
            "running": true,
            "games": games,
            "sessions": sessions
                .iter()
                .map(|s| json!({
                    "session_id": s.session_id,
                    "game_id": s.game_id,
                    "gamescope_pid": s.gamescope_pid,
                    "process_name": s.process_name,
                    "watch_only": s.watch_only,
                    "elapsed_secs": s.started_at.elapsed().as_secs(),
                }))
                .collect::<Vec<_>>(),
        }))
    }

    /// Which wine prefix would be used, and what was found on this machine.
    pub(super) async fn rpc_wine_status(&self) -> Result<Value, String> {
        let config = self.config.read().await;
        let detected = crate::wine::detect_prefixes(Path::new(""));
        Ok(json!({
            "configured": config.wine.prefix,
            "default": crate::wine::default_prefix(),
            "environment": std::env::var("WINEPREFIX").ok(),
            "detected": detected,
        }))
    }

    /// Set (or clear, with `null`) the machine-wide wine prefix.
    pub(super) async fn rpc_set_wine_prefix(&self, value: Value) -> Result<Value, String> {
        let prefix: Option<PathBuf> = match value {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => None,
            Value::String(text) => {
                let path = PathBuf::from(text.trim());
                // Accept a prefix that exists (must look like one) or a path
                // that does not exist yet (wine will populate it), but reject a
                // directory that clearly is not a prefix.
                if path.exists() && !path.join("drive_c").is_dir() {
                    return Err(format!(
                        "这不是一个 wine prefix（缺少 drive_c）: {}",
                        path.display()
                    ));
                }
                Some(path)
            }
            _ => return Err("prefix 必须是路径字符串或 null".to_string()),
        };

        self.mutate_config(|config| {
            config.wine.prefix = prefix.clone();
            tracing::info!("wine prefix set to {:?}", prefix);
            Ok(json!({ "prefix": prefix }))
        })
        .await
    }

    pub(super) async fn rpc_reload_config(&self) -> Result<Value, String> {
        let new_config = crate::config::load_at(&self.config_path).map_err(|e| e.to_string())?;
        *self.config.write().await = new_config;
        tracing::info!("configuration reloaded");
        Ok(json!({ "success": true }))
    }
}

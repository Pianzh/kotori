//! 真正搬存档的那几个 RPC 与会话钩子：测试连接、立即同步、列版本、恢复，以及
//! 启动前取回和退出后上传。
//!
//! 与 `credentials.rs` 分开：这里每一次调用都会碰网络与用户的存档，所以每一条
//! 都必须自带"失败时说什么"。

use serde_json::{Value, json};

use super::{Daemon, GameOutcome, PULL_TIMEOUT, SETTLE_DELAY, sync};

impl Daemon {
    /// Check credentials, bucket and write access.
    pub(in crate::daemon) async fn rpc_sync_test(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let remote = runner.check().await.map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "remote": remote }))
    }

    /// Upload now: one game, or every game that has save locations.
    pub(in crate::daemon) async fn rpc_sync_now(
        &self,
        game_id: Option<&str>,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;

        let ids: Vec<String> = match game_id {
            Some(id) => vec![id.to_string()],
            None => self
                .config
                .read()
                .await
                .games
                .iter()
                .filter(|(_, game)| !game.save_paths.is_empty())
                .map(|(id, _)| id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return Err("还没有任何游戏配置了存档位置".to_string());
        }

        let mut outcomes = Vec::with_capacity(ids.len());
        for id in ids {
            let (name, targets) = match self.sync_targets(&id).await {
                Ok(pair) => pair,
                Err(error) => {
                    outcomes.push(GameOutcome::failed(&id, &id, error));
                    continue;
                }
            };
            let outcome = runner.upload(&id, &name, &targets).await;
            self.sync.remember(&id, "上传", &outcome);
            outcomes.push(outcome);
        }

        Ok(json!({
            "ok": outcomes.iter().all(|o| o.ok),
            "games": outcomes,
        }))
    }

    /// The version packages the cloud holds for a game.
    pub(in crate::daemon) async fn rpc_sync_versions(
        &self,
        game_id: &str,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let versions = runner.packages(game_id).await.map_err(|e| e.to_string())?;
        Ok(json!({ "versions": versions }))
    }

    /// Put a game's saves back. Without `version`, the newest state wins.
    pub(in crate::daemon) async fn rpc_sync_restore(
        &self,
        game_id: &str,
        version: Option<&str>,
    ) -> Result<Value, String> {
        // A bad snapshot name is invalid input, not a sync failure: reject it
        // here, before anything runs, and as a JSON-RPC error.
        if let Some(version) = version
            && !sync::is_snapshot(version)
        {
            return Err(format!(
                "不是合法的版本名: {version}（形如 20260911T101500Z）"
            ));
        }

        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let (name, targets) = self.sync_targets(game_id).await?;

        let outcome = runner.restore(game_id, &name, &targets, version).await;
        self.sync.remember(game_id, "恢复", &outcome);
        Ok(json!({ "ok": outcome.ok, "game": outcome }))
    }

    /// Pull the newest cloud state before a game starts.
    ///
    /// Returns `None` when there is nothing to do (sync off, no save paths, no
    /// rclone). Any failure is reported in the returned object and **never**
    /// stops the launch: the user asked to play a game.
    pub(in crate::daemon) async fn sync_pull_before_launch(&self, game_id: &str) -> Option<Value> {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return None;
            }
            let game = config.games.get(game_id)?;
            if game.save_paths.is_empty() {
                return None;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => return Some(json!({ "ok": false, "error": error })),
        };

        let outcome =
            match tokio::time::timeout(PULL_TIMEOUT, runner.pull(game_id, &name, &targets)).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    tracing::warn!("{game_id}: 启动前拉取超时（{PULL_TIMEOUT:?}），直接启动游戏");
                    return Some(json!({
                        "ok": false,
                        "error": format!("拉取超过 {} 秒，已跳过", PULL_TIMEOUT.as_secs()),
                    }));
                }
            };

        self.sync.remember(game_id, "取回", &outcome);
        if !outcome.ok {
            tracing::warn!("{game_id}: 启动前拉取失败: {:?}", outcome.error);
        }
        Some(serde_json::to_value(&outcome).unwrap_or(Value::Null))
    }

    /// Upload after a game exits.
    ///
    /// Runs detached from the session watcher: an upload must never hold up the
    /// engine, and the daemon may be asked to shut down while it runs.
    pub(in crate::daemon) async fn sync_after_game_exit(&self, game_id: &str) {
        let settings = {
            let config = self.config.read().await;
            if !config.sync.enabled {
                return;
            }
            config.sync.clone()
        };

        let (name, targets) = match self.sync_targets(game_id).await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::debug!("{game_id}: 跳过退出后上传: {error}");
                return;
            }
        };
        let runner = match self.sync_runner(&settings) {
            Ok(runner) => runner,
            Err(error) => {
                tracing::warn!("{game_id}: 退出后上传失败: {error}");
                return;
            }
        };

        // Let wineserver finish flushing whatever the game just wrote.
        tokio::time::sleep(SETTLE_DELAY).await;

        let outcome = runner.upload(game_id, &name, &targets).await;
        self.sync.remember(game_id, "上传", &outcome);
        if outcome.ok {
            tracing::info!("{game_id}: 退出后已同步存档");
        } else {
            tracing::warn!("{game_id}: 退出后同步失败: {:?}", outcome.error);
        }
    }
}

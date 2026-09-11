use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, RwLock};

use crate::config::{self, Config};
use crate::scale::niri::NiriScaleEngine;
use crate::scale::{ScaleEngine, ScaleSession};

/// Daemon log file name inside [`config::log_dir`].
pub const DAEMON_LOG: &str = "daemon.log";

pub struct Daemon {
    config: Arc<RwLock<Config>>,
    /// Single source of truth for live sessions. There is deliberately no
    /// second session list here: a duplicate copy used to go stale and report
    /// already-exited games as running.
    engine: Arc<NiriScaleEngine>,
    shutdown: Arc<Notify>,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            engine: Arc::new(NiriScaleEngine::new()),
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let socket_path = {
            let config = self.config.read().await;
            config::resolve_socket(&config)
        };

        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Clean up any stale socket file.
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| anyhow::anyhow!("Failed to bind {}: {}", socket_path.display(), e))?;

        tracing::info!("daemon listening on {}", socket_path.display());

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let this = self.clone_shares();
                            tokio::spawn(async move {
                                if let Err(e) = this.handle_client(stream).await {
                                    tracing::warn!("client handler error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("accept error: {}", e);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                _ = self.shutdown.notified() => {
                    tracing::info!("shutdown requested, stopping daemon");
                    break;
                }
            }
        }

        drop(listener);
        let _ = std::fs::remove_file(&socket_path);
        tracing::info!("daemon stopped; running games (if any) keep running");
        Ok(())
    }

    /// Create a cheap clone of the shared state to move into a spawned task.
    fn clone_shares(&self) -> Arc<Self> {
        Arc::new(Daemon {
            config: self.config.clone(),
            engine: self.engine.clone(),
            shutdown: self.shutdown.clone(),
        })
    }

    async fn handle_client(&self, stream: UnixStream) -> anyhow::Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            tracing::debug!("IPC request: {}", line);
            let reply = self.handle_request(&line).await;

            // Flush the response *before* letting the accept loop exit, so
            // `daemon.shutdown` still gets a reply on the wire.
            writer.write_all(reply.body.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;

            if reply.shutdown {
                self.shutdown.notify_one();
            }
        }

        Ok(())
    }

    async fn handle_request(&self, raw: &str) -> Reply {
        let req: rpc::Request = match serde_json::from_str(raw) {
            Ok(req) => req,
            Err(e) => return rpc_err(Value::Null, -32700, format!("parse error: {e}")),
        };
        if req.jsonrpc != "2.0" {
            return rpc_err(
                req.id,
                -32600,
                format!("unsupported jsonrpc version: {:?}", req.jsonrpc),
            );
        }

        let id = req.id.clone();
        match req.method.as_str() {
            "daemon.status" => respond(id, self.rpc_status().await),
            "daemon.shutdown" => Reply {
                body: rpc_ok(id, json!({ "success": true })).body,
                shutdown: true,
            },
            "config.reload" => respond(id, self.rpc_reload_config().await),
            "game.list" => respond(id, self.rpc_game_list().await),
            "game.scan" => match param_str(&req.params, "directory") {
                Ok(dir) => respond(id, self.rpc_game_scan(dir).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.add" => match param_str(&req.params, "directory") {
                Ok(dir) => respond(id, self.rpc_game_add(dir).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.remove" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_game_remove(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.update" => match param_str(&req.params, "id") {
                Ok(game_id) => {
                    let text = |key: &str| {
                        req.params
                            .as_ref()
                            .and_then(|p| p.get(key))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    };
                    let name = text("name");
                    let exe_path = text("exe_path");
                    respond(id, self.rpc_game_update(game_id, name, exe_path).await)
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.launch" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_game_launch(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.wait" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_game_wait(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.stop" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_game_stop(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.get_status" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_status(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.toggle_fsr" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_toggle_fsr(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.toggle_integer" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_toggle_integer(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.adjust_sharpness" => match param_str(&req.params, "session_id") {
                Ok(sid) => {
                    let delta = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("delta"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    respond(id, self.rpc_scale_adjust_sharpness(sid, delta).await)
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            other => rpc_err(id, -32601, format!("method not found: {other}")),
        }
    }

    async fn rpc_status(&self) -> Result<Value, String> {
        let games = self.config.read().await.games.len();
        let sessions = self.engine.list_sessions().await;
        Ok(json!({
            "running": true,
            "games": games,
            "sessions": sessions
                .iter()
                .map(|s| json!({
                    "session_id": s.session_id,
                    "gamescope_pid": s.gamescope_pid,
                    "elapsed_secs": s.started_at.elapsed().as_secs(),
                }))
                .collect::<Vec<_>>(),
        }))
    }

    async fn rpc_reload_config(&self) -> Result<Value, String> {
        let new_config = crate::config::load().map_err(|e| e.to_string())?;
        *self.config.write().await = new_config;
        tracing::info!("configuration reloaded");
        Ok(json!({ "success": true }))
    }

    /// Return every configured game as its full `GameConfig` plus `id`.
    ///
    /// Serializing the whole struct (instead of hand-picking fields) is what
    /// keeps clients from silently dropping `force_fullscreen`,
    /// `framerate_limit` and sharpness.
    async fn rpc_game_list(&self) -> Result<Value, String> {
        let config = self.config.read().await;
        let mut games: Vec<Value> = config
            .games
            .iter()
            .map(|(id, game)| {
                let mut value = serde_json::to_value(game).unwrap_or_else(|e| {
                    tracing::warn!("failed to serialize game {id}: {e}");
                    json!({})
                });
                if let Value::Object(map) = &mut value {
                    map.insert("id".to_string(), Value::String(id.clone()));
                }
                value
            })
            .collect();
        // HashMap iteration order is random; keep the library stable.
        games.sort_by(|a, b| {
            let key = |v: &Value| {
                v.get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            key(a).cmp(&key(b))
        });

        Ok(json!({ "games": games }))
    }

    /// Preview a directory scan without touching the config.
    async fn rpc_game_scan(&self, directory: &str) -> Result<Value, String> {
        let dir = require_directory(directory)?;
        let found = crate::game::scan(&dir).map_err(|e| e.to_string())?;

        let config = self.config.read().await;
        let games: Vec<Value> = found
            .iter()
            .map(|game| {
                let id = crate::game::generate_game_id(&game.name);
                json!({
                    "id": id,
                    "name": game.name,
                    "exe_path": game.exe_path,
                    "is_new": !config.games.contains_key(&id),
                })
            })
            .collect();

        Ok(json!({ "directory": dir, "games": games }))
    }

    /// Scan a directory and add the games that are not configured yet.
    async fn rpc_game_add(&self, directory: &str) -> Result<Value, String> {
        let dir = require_directory(directory)?;
        let found = crate::game::scan(&dir).map_err(|e| e.to_string())?;
        let found_count = found.len();

        self.mutate_config(|config| {
            let added = crate::game::add_games(config, found);
            tracing::info!(
                "game.add: {} new game(s) from {}",
                added.len(),
                dir.display()
            );
            Ok(json!({
                "directory": dir,
                "found": found_count,
                "added": added
                    .iter()
                    .map(|(id, game)| json!({
                        "id": id,
                        "name": game.name,
                        "exe_path": game.exe_path,
                    }))
                    .collect::<Vec<_>>(),
            }))
        })
        .await
    }

    async fn rpc_game_remove(&self, id: &str) -> Result<Value, String> {
        self.mutate_config(|config| {
            if !crate::game::remove_game(config, id) {
                return Err(format!("配置中找不到游戏: {id}"));
            }
            tracing::info!("game.remove: {id}");
            Ok(json!({ "success": true }))
        })
        .await
    }

    /// Update the mutable library fields of a game (currently name / exe path).
    /// This is the repair path for a wrong exe picked by the scanner.
    async fn rpc_game_update(
        &self,
        id: &str,
        name: Option<String>,
        exe_path: Option<String>,
    ) -> Result<Value, String> {
        if name.is_none() && exe_path.is_none() {
            return Err("game.update 需要 name 或 exe_path 之一".to_string());
        }

        self.mutate_config(|config| {
            let game = config
                .games
                .get_mut(id)
                .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;

            if let Some(name) = &name {
                if name.trim().is_empty() {
                    return Err("名称不能为空".to_string());
                }
                game.name = name.clone();
            }

            if let Some(exe) = &exe_path {
                let path = PathBuf::from(exe);
                if !path.is_file() {
                    return Err(format!("可执行文件不存在: {}", path.display()));
                }
                game.exe_path = path;
            }

            tracing::info!("game.update: {id}");
            Ok(json!({ "success": true }))
        })
        .await
    }

    /// Apply a mutation to the config atomically: the change is made on a copy,
    /// persisted, and only then committed to memory, so the daemon's view never
    /// diverges from the file on disk.
    async fn mutate_config<F>(&self, mutate: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Config) -> Result<Value, String>,
    {
        let mut guard = self.config.write().await;
        let mut candidate = guard.clone();
        let value = mutate(&mut candidate)?;
        crate::config::save(&candidate).map_err(|e| format!("保存配置失败: {e}"))?;
        *guard = candidate;
        Ok(value)
    }

    async fn rpc_game_launch(&self, id: &str) -> Result<Value, String> {
        let game = {
            let config = self.config.read().await;
            config.games.get(id).cloned()
        };

        let Some(game) = game else {
            return Err(format!("Game not found: {id}"));
        };

        let session = self
            .engine
            .start_session(&game.exe_path.to_string_lossy(), &[], &game.scale_profile)
            .await
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "session_id": session.session_id,
            "gamescope_pid": session.gamescope_pid,
        }))
    }

    async fn rpc_game_wait(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .wait_session(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "exited": true }))
    }

    async fn rpc_game_stop(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .stop_session(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "success": true }))
    }

    async fn rpc_scale_status(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        let status = self
            .engine
            .get_status(&session)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(status).map_err(|e| e.to_string())
    }

    async fn rpc_scale_toggle_fsr(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .toggle_fsr(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "success": true }))
    }

    /// gamescope has no external runtime API, so this surfaces the backend's
    /// "use the built-in hotkey" guidance as an error.
    async fn rpc_scale_toggle_integer(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .toggle_integer(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "success": true }))
    }

    /// See [`Self::rpc_scale_toggle_integer`].
    async fn rpc_scale_adjust_sharpness(
        &self,
        session_id: &str,
        delta: i32,
    ) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .adjust_sharpness(&session, delta)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "success": true }))
    }

    async fn lookup_session(&self, session_id: &str) -> Result<ScaleSession, String> {
        self.engine
            .get_session(session_id)
            .await
            .ok_or_else(|| format!("session not found: {session_id}"))
    }
}

pub async fn run() -> anyhow::Result<()> {
    let config = crate::config::load()?;
    let daemon = Daemon::new(config);
    daemon.run().await
}

/// Start the daemon in the background if it is not already running, and wait
/// until its Unix socket is reachable.
///
/// Both the GUI and the CLI need this: every game launch has to go through the
/// daemon, so whoever runs first must be able to boot it.
pub fn ensure_running(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    if UnixStream::connect(socket).is_ok() {
        return Ok(());
    }
    tracing::info!("daemon 未运行，正在启动...");

    let log_dir = config::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(DAEMON_LOG);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    std::process::Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log))
        // Own process group: the daemon must outlive signals sent to the
        // UI/CLI process group.
        .process_group(0)
        .spawn()
        .map_err(|e| anyhow::anyhow!("无法启动守护进程: {e}"))?;

    // ~5s budget; this runs before the event loop starts.
    for _ in 0..50 {
        if UnixStream::connect(socket).is_ok() {
            tracing::info!("daemon 已就绪");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!(
        "守护进程未在 5 秒内就绪（socket: {}，日志: {}）",
        socket.display(),
        log_path.display()
    )
}

/// A JSON-RPC response ready to be written to the wire.
struct Reply {
    body: String,
    /// Set by `daemon.shutdown`; the caller signals after flushing the reply.
    shutdown: bool,
}

fn rpc_ok(id: Value, result: Value) -> Reply {
    Reply {
        body: json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        shutdown: false,
    }
}

fn rpc_err(id: Value, code: i32, message: impl Into<String>) -> Reply {
    Reply {
        body: json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message.into() }
        })
        .to_string(),
        shutdown: false,
    }
}

fn respond(id: Value, result: Result<Value, String>) -> Reply {
    match result {
        Ok(value) => rpc_ok(id, value),
        Err(message) => rpc_err(id, -32000, message),
    }
}

/// Required string parameter, or a JSON-RPC `invalid params` message.
fn param_str<'a>(
    params: &'a Option<serde_json::Map<String, Value>>,
    key: &str,
) -> Result<&'a str, String> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing parameter: {key}"))
}

/// Validate a directory parameter before scanning it.
fn require_directory(directory: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(directory);
    if !path.is_dir() {
        return Err(format!("目录不存在或不是目录: {}", path.display()));
    }
    Ok(path)
}

pub mod rpc {
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    pub struct Request {
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(default)]
        pub params: Option<serde_json::Map<String, Value>>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> Daemon {
        Daemon::new(Config::default())
    }

    #[tokio::test]
    async fn unknown_method_is_reported_as_method_not_found() {
        let reply = daemon()
            .handle_request(r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["error"]["code"], -32601);
        assert_eq!(value["id"], 7);
        assert!(!reply.shutdown);
    }

    #[tokio::test]
    async fn malformed_json_is_a_parse_error() {
        let reply = daemon().handle_request("{not json").await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn wrong_jsonrpc_version_is_rejected() {
        let reply = daemon()
            .handle_request(r#"{"jsonrpc":"1.0","id":1,"method":"game.list"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn missing_params_are_invalid_params() {
        let reply = daemon()
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.launch"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["error"]["code"], -32602);
        assert!(value["error"]["message"].as_str().unwrap().contains("id"));
    }

    #[tokio::test]
    async fn shutdown_reply_is_flagged() {
        let reply = daemon()
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.shutdown"}"#)
            .await;
        assert!(reply.shutdown);
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["result"]["success"], true);
    }

    #[tokio::test]
    async fn game_list_exposes_the_full_scale_profile() {
        // Regression test: a hand-picked subset silently dropped sharpness,
        // fullscreen and framerate, so saving from the UI overwrote them.
        let mut config = Config::default();
        let profile = crate::config::ScaleProfile {
            algorithm: crate::config::ScaleAlgorithm::Nis { sharpness: 4 },
            framerate_limit: Some(60),
            force_fullscreen: false,
            ..crate::config::ScaleProfile::default_for((1920, 1080))
        };
        config.games.insert(
            "demo".into(),
            crate::config::GameConfig {
                name: "demo".into(),
                exe_path: "/games/demo/game.exe".into(),
                save_paths: Vec::new(),
                scale_profile: profile,
                wine_prefix: None,
                created_at: chrono::Utc::now(),
            },
        );

        let reply = Daemon::new(config)
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.list"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();

        let game = &value["result"]["games"][0];
        assert_eq!(game["id"], "demo");
        assert_eq!(game["scale_profile"]["algorithm"]["Nis"]["sharpness"], 4);
        assert_eq!(game["scale_profile"]["framerate_limit"], 60);
        assert_eq!(game["scale_profile"]["force_fullscreen"], false);
        assert_eq!(game["scale_profile"]["output_width"], 1920);
    }

    #[tokio::test]
    async fn games_are_sorted_by_name() {
        let mut config = Config::default();
        for name in ["zeta", "alpha", "mid"] {
            config.games.insert(
                name.into(),
                crate::config::GameConfig {
                    name: name.into(),
                    exe_path: "/g/game.exe".into(),
                    save_paths: Vec::new(),
                    scale_profile: crate::config::ScaleProfile::default_for((1920, 1080)),
                    wine_prefix: None,
                    created_at: chrono::Utc::now(),
                },
            );
        }

        let reply = Daemon::new(config)
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.list"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        let names: Vec<&str> = value["result"]["games"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["alpha", "mid", "zeta"]);
    }

    #[tokio::test]
    async fn status_reports_no_sessions_initially() {
        let reply = daemon()
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.status"}"#)
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["result"]["running"], true);
        assert_eq!(value["result"]["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn status_on_unknown_session_lists_nothing_new() {
        // `scale.get_status` for a dead session must fail loudly instead of
        // reporting stale data from a cached copy.
        let reply = daemon()
            .handle_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"scale.get_status","params":{"session_id":"ghost"}}"#,
            )
            .await;
        let value: Value = serde_json::from_str(&reply.body).unwrap();
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("session not found")
        );
    }
}

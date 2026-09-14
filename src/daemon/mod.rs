use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, RwLock};

use crate::config::{self, Config};
use crate::scale::gamescope::GamescopeScaleEngine;
use crate::scale::{LaunchSpec, ScaleEngine, ScaleSession, SessionKind};

mod game_rpc;
mod protocol;
mod scale_rpc;
mod status_rpc;
mod sync_rpc;
use protocol::{GamePatch, NewGame, Reply, param_str, respond, rpc_err, rpc_ok};
use sync_rpc::SyncState;

/// Daemon log file name inside [`config::log_dir`].
pub const DAEMON_LOG: &str = "daemon.log";

pub struct Daemon {
    config: Arc<RwLock<Config>>,
    /// The config file this daemon owns. Remembered rather than re-resolved so
    /// a write can never land on a different file than the one we loaded.
    config_path: Arc<PathBuf>,
    /// Single source of truth for live sessions. There is deliberately no
    /// second session list here: a duplicate copy used to go stale and report
    /// already-exited games as running.
    engine: Arc<GamescopeScaleEngine>,
    shutdown: Arc<Notify>,
    /// Keyring handle and the last sync result per game.
    sync: Arc<SyncState>,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        Self::assemble(config, SyncState::system())
    }

    /// Build a daemon against an explicit secret store.
    ///
    /// Only tests need this today: production always uses the platform keyring,
    /// falling back to a session-only store (`SyncState::system`).
    #[cfg(test)]
    pub fn with_keyring(config: Config, keyring: crate::secrets::Keyring) -> Self {
        Self::assemble(config, SyncState::with_keyring(keyring))
    }

    /// A daemon with a session store and a chosen credential-file path.
    #[cfg(test)]
    pub fn with_keyring_at(
        config: Config,
        keyring: crate::secrets::Keyring,
        secrets_path: PathBuf,
    ) -> Self {
        Self::assemble(config, SyncState::with_keyring_at(keyring, secrets_path))
    }

    /// A daemon that finds an existing master-password file (a restart).
    #[cfg(test)]
    pub fn with_master_file(config: Config, secrets_path: PathBuf) -> Self {
        Self::assemble(config, SyncState::from_master_file(secrets_path))
    }

    fn assemble(config: Config, sync: SyncState) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: Arc::new(config::config_path()),
            engine: Arc::new(GamescopeScaleEngine::new()),
            shutdown: Arc::new(Notify::new()),
            sync: Arc::new(sync),
        }
    }

    /// Own an explicit config file instead of the machine-wide one. Tests use
    /// this so they never touch `~/.config/kotori/config.toml`.
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Arc::new(path.into());
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let socket_path = {
            let config = self.config.read().await;
            config::resolve_socket(&config)
        };

        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // One daemon per socket, enforced by a lock rather than by "the path
        // exists". Removing the file first — which is what this used to do — means
        // a second daemon **silently steals the socket from a live one**: the first
        // keeps running, keeps writing the config and keeps owning its games, but
        // nothing can reach it any more. Two writers on one `config.toml` is exactly
        // what "the daemon is the only writer" (ADR-002) rules out.
        let lock_path = socket_path.with_extension("lock");
        let _lock = claim_socket(&lock_path)?;

        // Now a stale socket file is all that can be left over: bind over it.
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| anyhow::anyhow!("Failed to bind {}: {}", socket_path.display(), e))?;

        tracing::info!("daemon listening on {}", socket_path.display());

        self.spawn_sync_events();

        // A logout or a shutdown stops this daemon with SIGTERM, and that is the
        // one exit where the games have to go with it. They live in the same
        // systemd scope, and systemd waits for every process in that scope —
        // including wine's `winedevice.exe`, which ignores SIGTERM and so costs
        // the whole `TimeoutStopSec` (90 s, measured twice on 2026-09-13).
        // Closing the sessions here takes seconds instead.
        //
        // `daemon.shutdown` deliberately still does *not* do this: a UI that quits
        // must not kill a running game (ADR-002), so the two exits stay distinct
        // and only the signal path tears games down.
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut signalled = false;

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
                _ = sigterm.recv() => {
                    tracing::info!("收到 SIGTERM（会话要结束了），把在跑的游戏一并收尾");
                    signalled = true;
                    break;
                }
                _ = sigint.recv() => {
                    tracing::info!("收到 SIGINT，把在跑的游戏一并收尾");
                    signalled = true;
                    break;
                }
            }
        }

        if signalled {
            self.close_all_sessions().await;

            // 会话之外还可能有没人认领的残留:上一次 daemon 被 SIGKILL 掉的那一局,
            // wine 的 `winedevice.exe` 会一直待在那儿 —— 它无视 SIGTERM、又不在任何
            // 我们能杀的进程组或进程树里,只有 `wineserver -k` 收得掉它。它是怎么变成
            // 90 秒关机的,见 `wine_prefixes` 的开头。
            crate::wine_prefixes::close_all().await;
        }

        drop(listener);
        let _ = std::fs::remove_file(&socket_path);
        if signalled {
            tracing::info!("daemon stopped; 在跑的游戏已一并收尾");
        } else {
            tracing::info!("daemon stopped; running games (if any) keep running");
        }
        Ok(())
    }

    /// Bring every live session down, for the one exit where that is right.
    ///
    /// A session's teardown already covers all three layers — process group,
    /// process tree, and wine's own server for that prefix — so this only has to
    /// find them. Without it the wine processes are orphaned into this daemon's
    /// systemd scope, and the machine cannot shut down until that scope times out.
    async fn close_all_sessions(&self) {
        let sessions = self.engine.list_sessions().await;
        if sessions.is_empty() {
            return;
        }

        tracing::info!("还有 {} 个会话在跑，逐一收尾", sessions.len());
        for session in sessions {
            if let Err(err) = self.engine.stop_session(&session).await {
                tracing::warn!("收尾会话 {} 失败：{err}", session.session_id);
            }
        }
    }

    /// Create a cheap clone of the shared state to move into a spawned task.
    fn clone_shares(&self) -> Arc<Self> {
        Arc::new(Daemon {
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            engine: self.engine.clone(),
            shutdown: self.shutdown.clone(),
            sync: self.sync.clone(),
        })
    }

    /// React to games starting and stopping.
    ///
    /// Save sync hangs off this: a game that exited gets its saves uploaded.
    /// Note that the event is only a *trigger* — the session map in the engine
    /// remains the single source of truth about what is running, and each
    /// upload runs in its own task so a slow network cannot stall the engine
    /// or the next event.
    fn spawn_sync_events(&self) {
        let Some(mut events) = self.engine.subscribe() else {
            tracing::debug!("this scale backend reports no session events");
            return;
        };
        let this = self.clone_shares();

        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) if event.kind == SessionKind::Ended => {
                        let Some(game_id) = event.game_id else {
                            continue;
                        };
                        let this = this.clone();
                        tokio::spawn(async move {
                            this.sync_after_game_exit(&game_id).await;
                        });
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!("错过了 {skipped} 个会话事件（同步可能少了触发）");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
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
        let req: protocol::rpc::Request = match serde_json::from_str(raw) {
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
            "wine.status" => respond(id, self.rpc_wine_status().await),
            "env.report" => respond(id, self.rpc_env_report().await),
            "wine.set_prefix" => {
                let value = match req.params.as_ref().and_then(|p| p.get("prefix")) {
                    Some(v) => v.clone(),
                    None => return rpc_err(id, -32602, "缺少参数: prefix".to_string()),
                };
                respond(id, self.rpc_set_wine_prefix(value).await)
            }

            "game.list" => respond(id, self.rpc_game_list().await),
            "game.remove" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_game_remove(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.create" => {
                match serde_json::from_value::<NewGame>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(new_game) => respond(id, self.rpc_game_create(new_game).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "game.update" => {
                let game_id = match param_str(&req.params, "id") {
                    Ok(v) => v.to_string(),
                    Err(e) => return rpc_err(id, -32602, e),
                };
                let mut patch_fields = req.params.clone().unwrap_or_default();
                patch_fields.remove("id");
                if patch_fields.is_empty() {
                    return rpc_err(
                        id,
                        -32602,
                        "game.update 需要至少一个要修改的字段".to_string(),
                    );
                }
                match serde_json::from_value::<GamePatch>(Value::Object(patch_fields)) {
                    Ok(patch) => respond(id, self.rpc_game_update(&game_id, patch).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
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
            "scale.action" => match (
                param_str(&req.params, "session_id"),
                param_str(&req.params, "action"),
            ) {
                (Ok(sid), Ok(action)) => match crate::scale::ScaleAction::from_id(action) {
                    Some(action) => respond(id, self.rpc_scale_action(sid, action).await),
                    None => rpc_err(id, -32602, format!("未知的缩放动作：{action}")),
                },
                (Err(e), _) | (_, Err(e)) => rpc_err(id, -32602, e),
            },
            "sync.status" => respond(id, self.rpc_sync_status().await),
            "sync.set_settings" => {
                match serde_json::from_value::<sync_rpc::SettingsPatch>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(patch) => respond(id, self.rpc_sync_set_settings(patch).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_credentials" => {
                match serde_json::from_value::<sync_rpc::Credentials>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(credentials) => respond(id, self.rpc_sync_set_credentials(credentials)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_password" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_set_password(password).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.unlock" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_unlock(password)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_master_password" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_set_master_password(password)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.clear_master_password" => respond(id, self.rpc_sync_clear_master_password()),
            "sync.lock" => respond(id, self.rpc_sync_lock()),
            "sync.test" => respond(id, self.rpc_sync_test().await),
            "sync.now" => {
                let game_id = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                respond(id, self.rpc_sync_now(game_id.as_deref()).await)
            }
            "sync.versions" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_sync_versions(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "sync.restore" => match param_str(&req.params, "id") {
                Ok(game_id) => {
                    let version = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    respond(id, self.rpc_sync_restore(game_id, version.as_deref()).await)
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            other => rpc_err(id, -32601, format!("method not found: {other}")),
        }
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
        crate::config::save_to(&self.config_path, &candidate)
            .map_err(|e| format!("保存配置失败: {e}"))?;
        *guard = candidate;
        Ok(value)
    }
}

/// Take the one-daemon-per-socket lock, or say who has it.
///
/// `flock` rather than "does the socket file exist": a leftover *file* is exactly
/// what the remove-then-bind below is for, while a **live** daemon holding this
/// lock must not be replaced. The kernel drops the lock when the holder goes away —
/// SIGKILL included — so a crashed daemon never blocks its successor and the lock
/// file itself can stay behind (it is empty, and it lives in the runtime dir).
fn claim_socket(lock_path: &Path) -> anyhow::Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| anyhow::anyhow!("无法创建锁文件 {}: {e}", lock_path.display()))?;
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !taken {
        anyhow::bail!(
            "已经有一个守护进程在跑（它占着 {}）。两个守护进程会同时写同一份配置，\
             所以这里不去抢它的 socket；要换掉它先跑 `kotori shutdown`。",
            lock_path.display()
        );
    }
    Ok(file)
}

pub async fn run() -> anyhow::Result<()> {
    let path = crate::config::config_path();
    let config = crate::config::load_at(&path)?;
    let daemon = Daemon::new(config).with_config_path(path);
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
            // 显式填过的输出尺寸(留空＝自动,所以这里必须自己给),顺手一起验证
            // 它不会在 RPC 上被丢掉。
            output_width: Some(1920),
            output_height: Some(1080),
            ..crate::config::ScaleProfile::default_for()
        };
        config.games.insert(
            "demo".into(),
            crate::config::GameConfig {
                name: "demo".into(),
                game_dir: "/games/demo".into(),
                exe_path: "/games/demo/game.exe".into(),
                launch_args: Vec::new(),
                save_paths: Vec::new(),
                wine_prefix: None,
                watch_only: false,
                process_name: None,
                scale_profile: profile,
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
                    game_dir: "/g".into(),
                    exe_path: "/g/game.exe".into(),
                    launch_args: Vec::new(),
                    save_paths: Vec::new(),
                    wine_prefix: None,
                    watch_only: false,
                    process_name: None,
                    scale_profile: crate::config::ScaleProfile::default_for(),
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

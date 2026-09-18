use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, RwLock};

use crate::config::{self, Config};
use crate::scale::{LaunchSpec, PlatformEngine, ScaleEngine, ScaleSession, SessionKind};

mod game_rpc;
mod ipc;
mod protocol;
mod scale_rpc;
mod status_rpc;
mod sync_rpc;
use protocol::{GamePatch, NewGame, Reply, param_str, respond, rpc_err, rpc_ok};
use sync_rpc::SyncState;

pub use ipc::ensure_running;

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
    engine: Arc<PlatformEngine>,
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
            engine: Arc::new(PlatformEngine::new()),
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

        // One daemon per endpoint, enforced by a lock rather than by "the path
        // exists". Removing the file first — which is what this used to do — means
        // a second daemon **silently steals the socket from a live one**: the first
        // keeps running, keeps writing the config and keeps owning its games, but
        // nothing can reach it any more. Two writers on one `config.toml` is exactly
        // what "the daemon is the only writer" (ADR-002) rules out.
        let _lock = ipc::claim_socket(&ipc::lock_path(&socket_path))?;

        let mut listener = ipc::Listener::bind(&socket_path).await?;

        tracing::info!("daemon listening on {}", socket_path.display());

        // 上一次 daemon 要是被杀或崩了，`stage-*` 会留在工作目录里（`Drop` 跑不到）。
        // 这在 Windows 上尤其要紧:`%TEMP%` 不像 Linux 那样有人定期扫,而这些目录还在
        // 数据目录下,更没人管 —— 一个包小的几 MB、大的上百 MB,攒着就是白占磁盘。
        // **此刻清是安全的**:锁已经在手,没有别的实例;同步也只在 daemon 里做
        // (CLI 的 `kotori sync` 是发 RPC 过来的)。
        let swept = crate::sync::runner::sweep_stale(&crate::sync::runner::default_work_dir());
        if swept > 0 {
            tracing::info!("清掉了上次留下的 {swept} 个临时包目录");
        }

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
        let mut signalled = false;

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok(stream) => {
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
                how = session_end_signal() => {
                    tracing::info!("收到 {how}（会话要结束了），把在跑的游戏一并收尾");
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

    /// 一个客户端连接上的全部往来。
    ///
    /// 泛型是刻意的:守护进程这一侧不该知道传输是 Unix socket 还是命名管道
    /// (见 [`ipc`])。`tokio::io::split` 比 `UnixStream::into_split` 多一层锁,
    /// 但一条连接上只有一条 JSON-RPC 要读写,这点量级完全可以忽略。
    async fn handle_client<S>(&self, stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (reader, mut writer) = tokio::io::split(stream);
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
            "sync.set_kopia_password" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_set_kopia_password(password)),
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

pub async fn run() -> anyhow::Result<()> {
    let path = crate::config::config_path();
    let config = crate::config::load_at(&path)?;
    let daemon = Daemon::new(config).with_config_path(path);
    daemon.run().await
}

/// 等到"这个会话要结束了"这件事发生。
///
/// Unix 上是 SIGTERM(登出/关机走这条)或 SIGINT;Windows 上没有这两个信号,
/// 控制台 Ctrl-C 是唯一的对等物。返回值只给日志用 —— 两条路要做的事完全一样,
/// 所以 `run()` 里只有一个分支,不用往 `select!` 里塞 cfg。
#[cfg(unix)]
async fn session_end_signal() -> &'static str {
    // 注册失败只可能是"不在 tokio runtime 里",而唯一的调用点就在 `run()` 的
    // 事件循环里。
    let mut sigterm = signal(SignalKind::terminate()).expect("register SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("register SIGINT");
    tokio::select! {
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv() => "SIGINT",
    }
}

#[cfg(not(unix))]
async fn session_end_signal() -> &'static str {
    let _ = tokio::signal::ctrl_c().await;
    "Ctrl-C"
}

/// A JSON-RPC response ready to be written to the wire.
#[cfg(test)]
mod tests;

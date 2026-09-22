use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, RwLock};

use crate::config::{self, Config};
use crate::scale::{LaunchSpec, PlatformEngine, ScaleEngine, ScaleSession, SessionKind};

mod dispatch;
mod game_rpc;
mod ipc;
mod protocol;
mod scale_rpc;
mod status_rpc;
mod sync_rpc;
mod watch;
use sync_rpc::SyncState;

pub use ipc::ensure_running;

/// Daemon log file name inside [`config::log_dir`].
pub const DAEMON_LOG: &str = "daemon.log";

pub struct Daemon {
    config: Arc<RwLock<Config>>,
    /// The config file this daemon owns. Remembered rather than re-resolved so
    /// a write can never land on a different file than the one we loaded.
    ///
    /// **写得进去**是因为设置页那只「切到便携 / 切到默认」的按钮:切完内存里这份
    /// 配置不变,变的只是"以后存到哪" —— 而它是唯一写者,所以换路径也得由它来换
    /// (见 `status_rpc::rpc_config_set_source`)。
    config_path: Arc<RwLock<PathBuf>>,
    /// 这台机器上"配置能从哪儿来"的两个地点。生产恒为探测结果;测试注入临时目录,
    /// 免得去写二进制旁边那个真文件。
    config_sources: Arc<status_rpc::ConfigSources>,
    /// Single source of truth for live sessions. There is deliberately no
    /// second session list here: a duplicate copy used to go stale and report
    /// already-exited games as running.
    engine: Arc<PlatformEngine>,
    shutdown: Arc<Notify>,
    /// Keyring handle and the last sync result per game.
    sync: Arc<SyncState>,
    /// 用户亲手「停止」掉的自动追踪:`game_id` → **那一刻**这个进程名对应的 pid。
    ///
    /// 后台那圈轮询不许把同一次运行再认回来(否则点一次「停止」,两秒后会话自己长
    /// 回来),但下一个进程实例(新 pid)照样跟 —— 按 pid 记而不是按"名字消失过"记,
    /// 是因为游戏退出到用户重开可能快过一个轮询周期,而"没观察到空档"不该让这一款
    /// 从此不再被追踪。
    ignored_watch: Arc<RwLock<std::collections::HashMap<String, Vec<i32>>>>,
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
            config_path: Arc::new(RwLock::new(config::config_path())),
            config_sources: Arc::new(status_rpc::ConfigSources::detect()),
            engine: Arc::new(PlatformEngine::new()),
            shutdown: Arc::new(Notify::new()),
            sync: Arc::new(sync),
            ignored_watch: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Own an explicit config file instead of the machine-wide one. Tests use
    /// this so they never touch `~/.config/kotori/config.toml`.
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Arc::new(RwLock::new(path.into()));
        self
    }

    /// 指定"便携 / 默认"两个地点。**只有测试用** —— 生产走探测
    /// (`ConfigSources::detect`),而探测结果里那个"便携"地点是**测试二进制旁边**,
    /// 谁也不该往那儿写。
    #[cfg(test)]
    pub fn with_config_sources(mut self, portable: Option<PathBuf>, default: PathBuf) -> Self {
        self.config_sources = Arc::new(status_rpc::ConfigSources::at(portable, default));
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

        self.spawn_fingerprint_backfill();
        self.spawn_sync_events();
        // 自动追踪:不是 kotori 启动的游戏也要有一局记录(见 `watch`)。
        self.spawn_process_watch();

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
            config_sources: self.config_sources.clone(),
            engine: self.engine.clone(),
            shutdown: self.shutdown.clone(),
            sync: self.sync.clone(),
            ignored_watch: self.ignored_watch.clone(),
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
        let path = self.config_path.read().await.clone();
        crate::config::save_to(&path, &candidate).map_err(|e| format!("保存配置失败: {e}"))?;
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

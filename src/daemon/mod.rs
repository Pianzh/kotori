use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, RwLock};

use crate::config::{self, Config};
use crate::scale::niri::NiriScaleEngine;
use crate::scale::{LaunchSpec, ScaleEngine, ScaleSession, SessionKind};

mod sync_rpc;
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
    engine: Arc<NiriScaleEngine>,
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
            engine: Arc::new(NiriScaleEngine::new()),
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

        // Clean up any stale socket file.
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| anyhow::anyhow!("Failed to bind {}: {}", socket_path.display(), e))?;

        tracing::info!("daemon listening on {}", socket_path.display());

        self.spawn_sync_events();

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
            "wine.status" => respond(id, self.rpc_wine_status().await),
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
    async fn rpc_wine_status(&self) -> Result<Value, String> {
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
    async fn rpc_set_wine_prefix(&self, value: Value) -> Result<Value, String> {
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

    async fn rpc_reload_config(&self) -> Result<Value, String> {
        let new_config = crate::config::load_at(&self.config_path).map_err(|e| e.to_string())?;
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

    /// Create a library entry from explicit user input (the manual add path —
    /// no scanning heuristics involved).
    async fn rpc_game_create(&self, new_game: NewGame) -> Result<Value, String> {
        let name = new_game.name.trim().to_string();
        if name.is_empty() {
            return Err("名称不能为空".to_string());
        }
        if !new_game.exe_path.is_file() {
            return Err(format!("可执行文件不存在: {}", new_game.exe_path.display()));
        }

        let game_dir = match new_game.game_dir {
            Some(dir) if !dir.as_os_str().is_empty() => {
                if !dir.is_dir() {
                    return Err(format!("游戏目录不存在: {}", dir.display()));
                }
                dir
            }
            // Default to where the exe lives.
            _ => new_game
                .exe_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        };

        let id = crate::game::generate_game_id(&name);
        if id.is_empty() {
            return Err("这个名称无法生成合法的游戏 ID，请换一个".to_string());
        }

        let output = crate::display::primary_resolution_or((
            crate::config::FALLBACK_OUTPUT_WIDTH,
            crate::config::FALLBACK_OUTPUT_HEIGHT,
        ));

        self.mutate_config(|config| {
            if config.games.contains_key(&id) {
                return Err(format!("已存在同名游戏（ID: {id}）"));
            }
            config.games.insert(
                id.clone(),
                crate::config::GameConfig {
                    name: name.clone(),
                    game_dir: game_dir.clone(),
                    exe_path: new_game.exe_path.clone(),
                    launch_args: Vec::new(),
                    save_paths: Vec::new(),
                    wine_prefix: None,
                    watch_only: false,
                    process_name: None,
                    scale_profile: crate::config::ScaleProfile::default_for(output),
                    created_at: chrono::Utc::now(),
                },
            );
            tracing::info!("game.create: {id}");
            Ok(json!({ "id": id, "name": name }))
        })
        .await
    }

    /// Patch the mutable fields of a game. This is the only way a client
    /// persists game settings (the daemon is the single writer of the config).
    async fn rpc_game_update(&self, id: &str, patch: GamePatch) -> Result<Value, String> {
        self.mutate_config(|config| {
            // Save paths are validated by resolving them, which needs an
            // immutable view of the game *and* the config; take that before
            // mutating anything.
            if let Some(save_paths) = &patch.save_paths {
                let snapshot = config
                    .games
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;
                let mut candidate = snapshot;
                if let Some(dir) = &patch.game_dir {
                    candidate.game_dir = dir.clone();
                }
                let (root, _) = crate::wine::SaveRoot::for_platform(&candidate, config);
                let game_dir = candidate.effective_game_dir();
                for save in save_paths {
                    crate::wine::resolve_save_path(&root, &game_dir, save)?;
                }
            }

            let game = config
                .games
                .get_mut(id)
                .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;

            if let Some(name) = &patch.name {
                if name.trim().is_empty() {
                    return Err("名称不能为空".to_string());
                }
                game.name = name.clone();
            }

            if let Some(dir) = &patch.game_dir {
                if !dir.is_dir() {
                    return Err(format!("游戏目录不存在: {}", dir.display()));
                }
                game.game_dir = dir.clone();
            }

            if let Some(exe) = &patch.exe_path {
                if !exe.is_file() {
                    return Err(format!("可执行文件不存在: {}", exe.display()));
                }
                game.exe_path = exe.clone();
            }

            if let Some(args) = &patch.launch_args {
                game.launch_args = args.clone();
            }

            if let Some(save_paths) = &patch.save_paths {
                game.save_paths = save_paths.clone();
            }

            // `null` clears an optional field; an absent key leaves it alone.
            if let Some(prefix) = &patch.wine_prefix {
                game.wine_prefix = prefix.clone();
            }
            if let Some(process_name) = &patch.process_name {
                game.process_name = process_name.clone().filter(|name| !name.trim().is_empty());
            }

            if let Some(watch_only) = patch.watch_only {
                game.watch_only = watch_only;
            }

            if let Some(profile) = &patch.profile {
                let mut parsed = profile.clone();
                parsed.normalize();
                parsed.validate()?;
                game.scale_profile = parsed;
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
        crate::config::save_to(&self.config_path, &candidate)
            .map_err(|e| format!("保存配置失败: {e}"))?;
        *guard = candidate;
        Ok(value)
    }

    async fn rpc_game_launch(&self, id: &str) -> Result<Value, String> {
        let (game, wine_prefix, prefix_source) = {
            let config = self.config.read().await;
            let Some(game) = config.games.get(id).cloned() else {
                return Err(format!("Game not found: {id}"));
            };
            let (prefix, source) = crate::wine::resolve_prefix(&game, &config);
            (game, prefix, source)
        };

        let game_dir = game.effective_game_dir();

        // Fetch the newest saves *before* the game can read them. Best effort on
        // a deadline: a broken backup must never keep the user out of their game
        // (see `sync_pull_before_launch`). `None` means sync had nothing to do.
        let pulled = self.sync_pull_before_launch(id).await;

        // Watch-only: kotori never launches these, it just follows the process
        // so clients (and save sync) know when the game runs.
        if !game.is_launchable() {
            let Some(name) = game.process_name.as_deref() else {
                return Err(format!(
                    "「{}」是「仅观测」模式，但没有填写要观测的进程名；请在详情页里补上",
                    game.name
                ));
            };
            let spec = LaunchSpec {
                game_id: id,
                exe: "",
                args: &[],
                game_dir: &game_dir,
                wine_prefix: None,
                profile: &game.scale_profile,
                process_name: Some(name),
                watch_only: true,
            };
            let session = self
                .engine
                .start_session(&spec)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(json!({
                "session_id": session.session_id,
                "watch_only": true,
                "process_name": name,
                "game_dir": game_dir,
                "sync_pull": pulled,
            }));
        }

        tracing::info!(
            "launching {} (cwd={} prefix={} ← {})",
            game.name,
            game_dir.display(),
            wine_prefix.display(),
            prefix_source.label()
        );

        let exe = game.exe_path.to_string_lossy().to_string();
        let spec = LaunchSpec {
            game_id: id,
            exe: &exe,
            args: &game.launch_args,
            game_dir: &game_dir,
            wine_prefix: Some(&wine_prefix),
            profile: &game.scale_profile,
            process_name: game.process_name.as_deref(),
            watch_only: false,
        };

        let session = self
            .engine
            .start_session(&spec)
            .await
            .map_err(|e| e.to_string())?;

        // Runtime scaling is gamescope's own shortcuts, injected through the
        // RemoteDesktop portal. A game is running now, which is exactly when
        // those hotkeys become useful — so ask for them here, once per daemon
        // run. Registration pops a consent dialog, hence the background task:
        // launching a game must not wait for it, and a missing portal only
        // means "no hotkeys".
        if crate::hotkeys::request_once(&crate::config::data_dir()) {
            tracing::info!("已向 portal 申请运行时缩放热键（需要你授权一次）");
        }

        Ok(json!({
            "session_id": session.session_id,
            "gamescope_pid": session.gamescope_pid,
            "game_dir": game_dir,
            "wine_prefix": wine_prefix,
            "prefix_source": prefix_source.label(),
            "sync_pull": pulled,
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

/// Fields a client may patch on an existing game. Absent keys are left alone;
/// `null` clears an optional field.
#[derive(Debug, Default, Deserialize)]
struct GamePatch {
    name: Option<String>,
    game_dir: Option<PathBuf>,
    exe_path: Option<PathBuf>,
    launch_args: Option<Vec<String>>,
    save_paths: Option<Vec<crate::config::SavePath>>,
    #[serde(default, deserialize_with = "double_option")]
    wine_prefix: Option<Option<PathBuf>>,
    watch_only: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    process_name: Option<Option<String>>,
    profile: Option<crate::config::ScaleProfile>,
}

/// Explicit input for the manual "add game" path.
#[derive(Debug, Deserialize)]
struct NewGame {
    name: String,
    exe_path: PathBuf,
    #[serde(default)]
    game_dir: Option<PathBuf>,
}

/// Tell `null` apart from "key absent" for `Option<Option<T>>` fields.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
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
                    scale_profile: crate::config::ScaleProfile::default_for((1920, 1080)),
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

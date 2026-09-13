//! Daemon calls: the payloads of the `Task::perform` futures the UI awaits.
//! They hold no state — they talk to the daemon over IPC and hand back what the
//! `Message` variants expect.

use super::*;

/// Load the library, booting the daemon first if it is not running. Used for
/// both the initial load and automatic reconnect.
pub(super) async fn connect_and_load() -> Result<Vec<UiGame>, String> {
    let socket = crate::config::socket_path();
    match load_games_from(&socket).await {
        Ok(games) => Ok(games),
        Err(first) => {
            let boot = socket.clone();
            let booted = tokio::task::spawn_blocking(move || crate::daemon::ensure_running(&boot))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("启动守护进程的任务失败: {e}")));
            match booted {
                Ok(()) => load_games_from(&socket).await,
                Err(_) => Err(first),
            }
        }
    }
}

/// Look at the library **without** starting anything.
///
/// This is what the periodic refresh uses once the user has stopped the service
/// by hand: a 刷新 must not quietly undo the thing they just asked for. (An
/// explicit action that needs the daemon — adding a game, saving a profile —
/// still boots it; that is the user asking, not the UI deciding.)
pub(super) async fn load_without_booting() -> Result<Vec<UiGame>, String> {
    load_games_from(&crate::config::socket_path()).await
}

pub(super) async fn load_games_from(socket: &Path) -> Result<Vec<UiGame>, String> {
    let value = crate::rpc::call(socket, "game.list", None).await?;
    parse_games(&value)
}

/// Add one game from explicit user input.
pub(super) async fn create_game(
    socket: &Path,
    name: String,
    exe_path: String,
    game_dir: String,
) -> Result<String, String> {
    let mut params = vec![
        ("name", Value::String(name)),
        ("exe_path", Value::String(exe_path)),
    ];
    let game_dir = game_dir.trim();
    if !game_dir.is_empty() {
        params.push(("game_dir", Value::String(game_dir.to_string())));
    }

    let value = crate::rpc::call(socket, "game.create", Some(crate::rpc::params(params))).await?;
    Ok(value
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string())
}

/// `Some(prefix)` sets the machine-wide wine prefix, `None` returns to
/// auto-detection.
pub(super) async fn set_wine_prefix(socket: &Path, prefix: Option<String>) -> Result<(), String> {
    let value = match prefix {
        Some(prefix) => Value::String(prefix),
        None => Value::Null,
    };
    crate::rpc::call(
        socket,
        "wine.set_prefix",
        Some(crate::rpc::params([("prefix", value)])),
    )
    .await?;
    Ok(())
}

pub(super) async fn load_wine_status() -> Result<WineStatus, String> {
    let value = crate::rpc::call(&crate::config::socket_path(), "wine.status", None).await?;
    Ok(parse_wine_status(&value))
}

/// 设置页「环境检查」:让 daemon 去探一遍依赖(`env.report`)。
///
/// 只在设置页被打开和用户点「重新检查」时问 —— 那一步会真去跑几个 `--version`、
/// 建一次 portal 代理、问一次密钥环,不能跟着每 3 秒的状态轮询一起跑。
pub(super) async fn load_environment() -> Result<Environment, String> {
    let value = crate::rpc::call(&crate::config::socket_path(), "env.report", None).await?;
    Ok(parse_environment(&value))
}

/// 设置页的「启动服务」:把守护进程拉起来。`ensure_running` 是阻塞的(它会等
/// socket 就绪),所以丢进 blocking 线程池,别把界面卡住。
pub(super) async fn start_daemon() -> Result<String, String> {
    let socket = crate::config::socket_path();
    tokio::task::spawn_blocking(move || crate::daemon::ensure_running(&socket))
        .await
        .map_err(|e| format!("启动守护进程的任务失败: {e}"))?
        .map_err(|e| e.to_string())?;
    Ok("后台服务已启动".to_string())
}

/// 设置页的「停止服务」。
///
/// 走 `daemon.shutdown` 而不是发信号:这一条**故意不碰正在跑的游戏**(ADR-002 的
/// 两个出口是分开的 —— 只有系统发的 SIGTERM 才会把游戏一并收尾)。所以玩家自己
/// 点这个按钮时,那一局照常玩到退出。
pub(super) async fn stop_daemon() -> Result<String, String> {
    crate::rpc::call(&crate::config::socket_path(), "daemon.shutdown", None).await?;
    Ok("后台服务已停止".to_string())
}

/// Read the cloud-sync status. Secrets are never returned by the daemon, so
/// this can be held in the UI without any caution.
pub(super) async fn load_sync_status() -> Result<SyncStatus, String> {
    let socket = crate::config::socket_path();
    let value = crate::rpc::call(&socket, "sync.status", None).await?;
    parse_sync_status(&value)
}

/// Persist the sync settings. The daemon validates and may refuse (an
/// unconfirmed encryption change, an impossible prefix), so its message is
/// surfaced verbatim.
pub(super) async fn save_sync_settings(socket: &Path, patch: Value) -> Result<(), String> {
    let params = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "内部错误：设置补丁不是对象".to_string())?;
    crate::rpc::call(socket, "sync.set_settings", Some(params)).await?;
    Ok(())
}

pub(super) async fn save_sync_credentials(
    socket: &Path,
    key_id: &str,
    app_key: &str,
) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.set_credentials",
        Some(crate::rpc::params([
            ("key_id", Value::String(key_id.to_string())),
            ("app_key", Value::String(app_key.to_string())),
        ])),
    )
    .await?;
    Ok(())
}

pub(super) async fn save_sync_password(socket: &Path, password: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.set_password",
        Some(crate::rpc::params([
            ("password", Value::String(password.to_string())),
            // Changing an existing password under encryption is confirmed in
            // the UI; the daemon only insists on an explicit intent.
            ("force", Value::Bool(true)),
        ])),
    )
    .await?;
    Ok(())
}

/// Unlock the master-password file. The password goes over IPC to our own
/// daemon and is never written anywhere.
pub(super) async fn unlock_credentials(socket: &Path, password: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.unlock",
        Some(crate::rpc::params([(
            "password",
            Value::String(password.to_string()),
        )])),
    )
    .await?;
    Ok(())
}

/// 锁上凭据文件:派生出来的密钥从内存里丢掉,再想看凭据就得重新输主密码。
pub(super) async fn lock_credentials(socket: &Path) -> Result<(), String> {
    crate::rpc::call(socket, "sync.lock", None).await?;
    Ok(())
}

/// 删掉主密码凭据文件。里面的凭据一起消失 —— 忘了主密码时这是唯一的出路,
/// 所以它不需要先解锁(见 `rpc_sync_clear_master_password`)。
pub(super) async fn clear_master_file(socket: &Path) -> Result<(), String> {
    crate::rpc::call(socket, "sync.clear_master_password", None).await?;
    Ok(())
}

/// Seal the current credentials into a master-password file, and say where it
/// landed.
pub(super) async fn set_master_password(socket: &Path, password: &str) -> Result<String, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_master_password",
        Some(crate::rpc::params([
            ("password", Value::String(password.to_string())),
            // The UI asked for the password in a dedicated field; that is the
            // confirmation.
            ("force", Value::Bool(true)),
        ])),
    )
    .await?;
    Ok(str_field(&value, "path"))
}

pub(super) async fn sync_test(socket: &Path) -> Result<String, String> {
    let value = crate::rpc::call(socket, "sync.test", None).await?;
    Ok(str_field(&value, "remote"))
}

/// Upload now, and turn the daemon's per-location report into one line.
pub(super) async fn sync_now(socket: &Path, game_id: Option<String>) -> Result<String, String> {
    let params = crate::rpc::params(game_id.map(|id| ("id", Value::String(id))));
    let value = crate::rpc::call(socket, "sync.now", Some(params)).await?;
    Ok(describe_sync_outcome(&value))
}

pub(super) async fn sync_restore(
    socket: &Path,
    game_id: &str,
    version: Option<&str>,
) -> Result<String, String> {
    let mut params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    if let Some(version) = version {
        params.insert("version".into(), Value::String(version.to_string()));
    }
    let value = crate::rpc::call(socket, "sync.restore", Some(params)).await?;
    Ok(describe_sync_outcome(&value["game"]))
}

/// Live sessions, keyed by game id.
pub(super) async fn load_status(
    socket: &Path,
) -> Result<std::collections::BTreeMap<String, SessionInfo>, String> {
    let value = crate::rpc::call(socket, "daemon.status", None).await?;
    let mut running = std::collections::BTreeMap::new();

    if let Some(sessions) = value.get("sessions").and_then(|v| v.as_array()) {
        for session in sessions {
            let (Some(game_id), Some(session_id)) = (
                session.get("game_id").and_then(|v| v.as_str()),
                session.get("session_id").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            running.insert(
                game_id.to_string(),
                SessionInfo {
                    session_id: session_id.to_string(),
                    watch_only: session
                        .get("watch_only")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                },
            );
        }
    }

    Ok(running)
}

pub(super) async fn stop_session(socket: &Path, session_id: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "game.stop",
        Some(crate::rpc::params([(
            "session_id",
            Value::String(session_id.to_string()),
        )])),
    )
    .await?;
    Ok(())
}

pub(super) async fn remove_game(socket: &Path, game_id: &str) -> Result<(), String> {
    let params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    crate::rpc::call(socket, "game.remove", Some(params)).await?;
    Ok(())
}

/// Persist the whole edit form through the daemon, which is the single writer
/// of the config file.
pub(super) async fn save_profile(draft: Draft) -> Result<(), String> {
    let profile = profile_from_draft(&draft)?;

    if draft.exe.trim().is_empty() {
        return Err("可执行文件路径不能为空".to_string());
    }

    let mut params = vec![
        ("id", Value::String(draft.game_id.clone())),
        (
            "profile",
            serde_json::to_value(&profile).map_err(|e| e.to_string())?,
        ),
    ];
    // Only send paths that actually changed: the daemon rejects a path that
    // does not exist, and a game on an unmounted drive must not block a scale
    // edit.
    if draft.game_dir_changed() {
        params.push(("game_dir", Value::String(draft.game_dir.trim().to_string())));
    }
    if draft.exe_changed() {
        params.push(("exe_path", Value::String(draft.exe.trim().to_string())));
    }
    if draft.save_paths_changed() {
        params.push(("save_paths", save_paths_to_json(&draft.save_paths)));
    }

    crate::rpc::call(
        &crate::config::socket_path(),
        "game.update",
        Some(crate::rpc::params(params)),
    )
    .await?;
    Ok(())
}

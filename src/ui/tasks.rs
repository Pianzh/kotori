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

/// Add one game from explicit user input. The second element of the payload is
/// the daemon's duplicate-exe warning (`game.create` still adds the entry —
/// "警告但不阻止").
pub(super) async fn create_game(
    socket: &Path,
    name: String,
    exe_path: String,
    game_dir: String,
) -> Result<(String, Option<String>), String> {
    let mut params = vec![
        ("name", Value::String(name)),
        ("exe_path", Value::String(exe_path)),
    ];
    let game_dir = game_dir.trim();
    if !game_dir.is_empty() {
        params.push(("game_dir", Value::String(game_dir.to_string())));
    }

    let value = crate::rpc::call(socket, "game.create", Some(crate::rpc::params(params))).await?;
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let warning = value
        .get("warning")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((id, warning))
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
/// impossible prefix, say), so its message is surfaced verbatim.
pub(super) async fn save_sync_settings(socket: &Path, patch: Value) -> Result<bool, String> {
    let params = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "内部错误：设置补丁不是对象".to_string())?;
    let value = crate::rpc::call(socket, "sync.set_settings", Some(params)).await?;
    Ok(engine_changed(&value))
}

/// 只提交"用哪个引擎"这一个字段。
///
/// 引擎是个二选一的开关，点下去就该生效 —— 不该跟 bucket 那些字段一起等「保存设置」。
/// `sync.set_settings` 是**按字段合并**的（`SettingsPatch` 全是 `Option`），所以这里
/// 只发 engine 一个键，用户还没保存的其它编辑一个都不会被带上，也不会被当成已保存。
pub(super) async fn save_sync_engine(socket: &Path, engine: &str) -> Result<bool, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_settings",
        Some(crate::rpc::params([(
            "engine",
            Value::String(engine.to_string()),
        )])),
    )
    .await?;
    Ok(engine_changed(&value))
}

/// daemon 在 `sync.set_settings` 的回包里说"这次换引擎了"。
///
/// 值得单独一个函数，是因为它带的是**一句必须说出口的警告**：换了引擎之后，另一个
/// 引擎传上去的版本不会显示出来（数据还在桶里，只是这边读不出来），而这件事**不会
/// 报错**。丢掉它，用户就只能自己发现"我的存档怎么不见了"—— 这一条从前是被
/// `Ok(())` 整个吞掉的。
fn engine_changed(value: &Value) -> bool {
    value
        .get("engine_changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
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

/// 设置（或清除）kopia 仓库密码。留空 = 清除 = 回到默认的 `kotori`。
///
/// 返回 `true` 表示"现在用的是默认密码"——这句话必须让用户看见：默认密码意味着
/// 任何拿到桶的人都能解开仓库。
pub(super) async fn save_kopia_password(socket: &Path, password: &str) -> Result<bool, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_kopia_password",
        Some(crate::rpc::params([(
            "password",
            Value::String(password.to_string()),
        )])),
    )
    .await?;
    Ok(value
        .get("using_default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
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

/// `daemon.status` 的一次回包:谁在跑,以及**配置现在放在哪**。
///
/// 两件事同一次往返:设置页那一组"配置放在哪 / 切一下"要的就是这条 RPC 里的
/// 两个字段(见 `status_rpc::rpc_status`),再单独发一次只是多一个空窗。
#[derive(Debug, Clone, Default)]
pub(crate) struct DaemonStatus {
    pub(crate) sessions: std::collections::BTreeMap<String, SessionInfo>,
    pub(crate) config: ConfigSource,
}

/// Live sessions and the config location, keyed by game id.
pub(super) async fn load_status(socket: &Path) -> Result<DaemonStatus, String> {
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

    let config = ConfigSource {
        path: value
            .get("config_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        portable_path: value
            .get("config_portable_path")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        pinned: value
            .get("config_pinned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };

    Ok(DaemonStatus {
        sessions: running,
        config,
    })
}

/// 「切到便携配置 / 切到默认配置」:让 daemon 把配置搬到另一个地点。
///
/// 搬完它自己就记住了新路径(唯一写者),**不需要重启**。回来那句话是给用户看的:
/// 说清搬到了哪里、被留下那份去哪了。
pub(super) async fn set_config_source(socket: &Path, portable: bool) -> Result<String, String> {
    let value = crate::rpc::call(
        socket,
        "config.set_source",
        Some(crate::rpc::params([("portable", Value::Bool(portable))])),
    )
    .await?;

    let path = value
        .get("config_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    if value.get("changed").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(format!("配置本来就在 {path}"));
    }
    let mut message = format!("已切换，配置现在存在 {path}（不用重启）");
    if let Some(moved) = value.get("moved").and_then(|v| v.as_str()) {
        message.push_str(&format!("；原来那份已改名为 {moved}"));
    }
    Ok(message)
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

/// 「从正在运行的进程里挑」的候选:打开浮层时取一次(过滤在内存里做)。
pub(super) async fn load_processes(socket: &Path) -> Result<Vec<ProcessRow>, String> {
    let value = crate::rpc::call(socket, "process.list", None).await?;
    parse_processes(&value)
}

/// 「跟这一局」:让 daemon 盯住用户挑的那个 pid(只对这一次运行有意义,不写配置)。
pub(super) async fn observe_process(
    socket: &Path,
    game_id: &str,
    pid: i32,
) -> Result<String, String> {
    let value = crate::rpc::call(
        socket,
        "game.observe",
        Some(crate::rpc::params([
            ("id", Value::String(game_id.to_string())),
            ("pid", Value::from(pid)),
        ])),
    )
    .await?;
    let name = value
        .get("process_name")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    Ok(format!(
        "已开始跟随 {name}（PID {pid}），退出后照常上传存档"
    ))
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
    // 额外参数按空白切成 argv —— 和 gamescope 自由参数同一条规则(见 `split_args`)。
    if draft.launch_args_changed() {
        let args = split_args(&draft.launch_args);
        params.push((
            "launch_args",
            serde_json::to_value(args).map_err(|e| e.to_string())?,
        ));
    }
    if draft.save_paths_changed() {
        params.push(("save_paths", save_paths_to_json(&draft.save_paths)));
    }
    if draft.direct_launch != draft.direct_launch_original {
        params.push(("direct_launch", Value::Bool(draft.direct_launch)));
    }
    if draft.auto_watch != draft.auto_watch_original {
        params.push(("auto_watch", Value::Bool(draft.auto_watch)));
    }
    if draft.process_name_changed() {
        // 空 = 回到"按 exe 文件名认"。daemon 那边的 `double_option` 要求**显式 null**
        // 才是"清掉"(键不出现 = 别动这个字段),所以这里必须发 Null 而不是空串。
        let name = draft.process_name.trim();
        params.push((
            "process_name",
            if name.is_empty() {
                Value::Null
            } else {
                Value::String(name.to_string())
            },
        ));
    }

    crate::rpc::call(
        &crate::config::socket_path(),
        "game.update",
        Some(crate::rpc::params(params)),
    )
    .await?;
    Ok(())
}

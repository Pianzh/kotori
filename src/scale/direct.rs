//! 无 gamescope 的两类会话:**观测**(watch_only,什么都不启动)与**直接启动**
//! (不走缩放,把游戏跑起来)。两个引擎共用 —— 会话登记、watcher 与 `Ended`
//! 事件只写一遍,而退出后自动上传恰恰挂在 `Ended` 上,两端的直启都必须发它。
//!
//! 平台差异只在那一个 spawn:Linux 直启 = wine(收尾时还要 `wineserver -k`),
//! Windows 直启 = 裸 exe(游戏窗口要显示,不套 `CREATE_NO_WINDOW`)。其余——
//! 盯 `process_name` 判结束、启动器交接后继续等——两端一模一样。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::process::Command;
use tokio::sync::{RwLock, broadcast};

use super::{LaunchSpec, ScaleError, ScaleSession, SessionEvent, SessionKind};
use crate::process;
use crate::util::executor::find_binary;

type Sessions = Arc<RwLock<HashMap<String, ScaleSession>>>;
type Events = broadcast::Sender<SessionEvent>;

/// Track a game the user starts themselves: kotori launches nothing, the
/// session simply follows `process_name` for as long as it runs.
pub(super) async fn start_watch_session(
    sessions: &Sessions,
    events: &Events,
    spec: &LaunchSpec<'_>,
) -> Result<ScaleSession, ScaleError> {
    let Some(name) = spec.process_name.filter(|name| !name.trim().is_empty()) else {
        return Err(ScaleError::ProtocolError(
            "「仅观测」模式必须指定要观测的进程名".to_string(),
        ));
    };

    let session = ScaleSession {
        session_id: uuid::Uuid::new_v4().to_string(),
        game_id: Some(spec.game_id.to_string()),
        gamescope_pid: None,
        profile: spec.profile.clone(),
        // 观测会话没有 gamescope,缩放相关的两个字段都没有意义;占位为 0/1.0,
        // scale 动作对它们一律拒绝(见 `apply_action` 的 direct 分支)。
        runtime_ratio: 1.0,
        started_at: std::time::Instant::now(),
        process_group: None,
        process_name: Some(name.to_string()),
        output_size: (0, 0),
        // Nothing was launched, so there is no prefix of ours to close.
        wine_prefix: None,
        watch_only: true,
        direct: false,
    };

    register(sessions, events, &session).await;
    tracing::info!(
        "watching for process {name} (session {})",
        session.session_id
    );

    spawn_watch_task(
        sessions.clone(),
        events.clone(),
        session.session_id.clone(),
        session.game_id.clone(),
        name.to_string(),
        None,
        None,
    );
    Ok(session)
}

/// 直接启动:spawn 游戏(不套 gamescope),登记会话并跟到它退出。
pub(super) async fn start_direct_session(
    sessions: &Sessions,
    events: &Events,
    spec: &LaunchSpec<'_>,
) -> Result<ScaleSession, ScaleError> {
    #[cfg(unix)]
    if find_binary("wine").is_none() {
        return Err(ScaleError::WineNotFound);
    }

    // The process that *is* the game: wine rewrites `argv[0]`, so the exe's
    // own file name is what matches it — unless the config names the process.
    let name = spec
        .process_name
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            Path::new(spec.exe)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .filter(|name| !name.trim().is_empty())
        });

    // Run with the *game root* as CWD: many visual novels resolve assets
    // relative to it.
    let cwd = if spec.game_dir.as_os_str().is_empty() {
        Path::new(spec.exe)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        spec.game_dir.to_path_buf()
    };

    let mut command = build_command(spec, &cwd);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(spawn_error)?;

    // Catch "exited immediately" (bad path, missing DLL, ...) instead of
    // reporting a session that dies before it was ever alive.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if let Ok(Some(status)) = child.try_wait() {
        return Err(ScaleError::StartFailed(format!(
            "进程立即退出（{status}）——检查可执行文件与它的工作目录"
        )));
    }

    let pid = child.id().unwrap_or(0);

    // 把这个 prefix 记进数据目录:万一这一局之后我们没机会收尾,下一个 daemon
    // 至少能在关机时把它关干净(见 `wine_prefixes`)。
    #[cfg(unix)]
    if let Some(prefix) = spec.wine_prefix {
        crate::wine_prefixes::record(prefix);
    }

    let session = ScaleSession {
        session_id: uuid::Uuid::new_v4().to_string(),
        game_id: Some(spec.game_id.to_string()),
        // 不是 gamescope,而是我们 spawn 的那个进程:`stop_session` 靠它杀树
        // (Linux),界面拿它显示"在跑"。
        gamescope_pid: Some(pid),
        profile: spec.profile.clone(),
        runtime_ratio: 1.0,
        started_at: std::time::Instant::now(),
        // Windows 没有进程组:没有可以一锅端的东西,停止 = 不再跟踪。
        process_group: if cfg!(unix) { Some(pid) } else { None },
        process_name: name.clone(),
        output_size: (0, 0),
        wine_prefix: if cfg!(unix) {
            spec.wine_prefix.map(Path::to_path_buf)
        } else {
            None
        },
        watch_only: false,
        direct: true,
    };

    register(sessions, events, &session).await;

    spawn_watch_task(
        sessions.clone(),
        events.clone(),
        session.session_id.clone(),
        session.game_id.clone(),
        name.clone().unwrap_or_default(),
        Some(child),
        session.wine_prefix.clone(),
    );
    Ok(session)
}

/// 平台各自的启动命令:Linux = `wine <exe> <args>`(带解析出的 prefix),
/// Windows = `<exe> <args>`(kotori 在那边不经过 wine)。wine 缺失由调用方先查。
fn build_command(spec: &LaunchSpec<'_>, cwd: &Path) -> Command {
    #[cfg(unix)]
    {
        let wine = find_binary("wine")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "wine".to_string());
        let mut command = Command::new(wine);
        command.arg(spec.exe).args(spec.args).current_dir(cwd);
        if let Some(prefix) = spec.wine_prefix {
            command.env("WINEPREFIX", prefix);
        }
        command
    }
    #[cfg(windows)]
    {
        let mut command = Command::new(spec.exe);
        command.args(spec.args).current_dir(cwd);
        command
    }
}

#[cfg(unix)]
fn spawn_error(e: std::io::Error) -> ScaleError {
    // wine 不在是最常见的那种失败,给一条说得清的。
    if find_binary("wine").is_none() {
        return ScaleError::WineNotFound;
    }
    ScaleError::StartFailed(format!("{e}"))
}

#[cfg(windows)]
fn spawn_error(e: std::io::Error) -> ScaleError {
    ScaleError::StartFailed(format!("{e}"))
}

async fn register(sessions: &Sessions, events: &Events, session: &ScaleSession) {
    sessions
        .write()
        .await
        .insert(session.session_id.clone(), session.clone());
    // A send error only means "no subscribers", which is fine.
    let _ = events.send(SessionEvent {
        session_id: session.session_id.clone(),
        game_id: session.game_id.clone(),
        kind: SessionKind::Started,
    });
}

/// 跟到游戏退出再发 `Ended`,两条会话路径共用。
///
/// `child` 是直启时我们 spawn 的进程(`None` = 观测)。两条路的**开头**不一样,
/// 也必须是两段:
///
/// * 直启:启动时已经确认那个进程活着(300ms 检测),所以不需要"等它出现" ——
///   等它退出就是这一局的尽头;退出时名字还在跑说明是启动器交接(真游戏还在),
///   继续跟到走光。这与 `gamescope.rs` 的收尾同构:那边 child 是 gamescope。
/// * 观测:kotori 什么都没启动,用户可能还没开游戏,所以先等名字出现(有上限)
///   再等它消失。
///
/// ⚠ 从前两条路共用同一个"先等出现"的开头,直启用它就错了:此时被盯的名字**就是**
/// 它刚 spawn 的那个 exe(没配 `process_name` 时),`wait()` 返回时名字当然已经不在,
/// 于是每次直启结束都要空等满 `APPEAR_TIMEOUT`(300 秒)才走"进程始终没出现,放弃"
/// 那条分支 —— 而那条分支**不发 `Ended`**,退出后的自动上传因此整条不触发
/// (Windows 上没有 gamescope,每个游戏都走直启)。
///
/// `prefix` 是这一局用的 wine prefix(观测恒为 `None`):游戏都走光之后关掉
/// wine 的那摊,否则 `winedevice.exe` 会把一次注销拖成 90 秒。
fn spawn_watch_task(
    sessions: Sessions,
    events: Events,
    sid: String,
    game_id: Option<String>,
    name: String,
    child: Option<tokio::process::Child>,
    prefix: Option<PathBuf>,
) {
    tokio::spawn(async move {
        if let Some(mut child) = child {
            match child.wait().await {
                Ok(status) => {
                    tracing::info!("session {sid}: 启动的进程已退出（{status}）");
                }
                Err(err) => tracing::warn!("session {sid}: 等进程结束出错：{err}"),
            }
        } else if !name.is_empty() {
            // 观测模式:kotori 什么都没启动,用户可能还没把游戏开起来,先等它出现。
            let deadline = tokio::time::Instant::now() + process::APPEAR_TIMEOUT;
            loop {
                if !sessions.read().await.contains_key(&sid) {
                    return;
                }
                if process::is_running(&name) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!("session {sid}: {name} never appeared, giving up");
                    sessions.write().await.remove(&sid);
                    // Deliberately no `Ended`: nothing ever ran, so a client
                    // must not treat this as "a game finished, sync it".
                    return;
                }
                tokio::time::sleep(process::POLL_INTERVAL).await;
            }
            tracing::info!("session {sid}: {name} is running");
        }

        // 两条路共用的收尾:等这个名字走光。启动器交接时,我们启动的那个先退,
        // 真游戏顶着这个名字还在跑 —— 所以这一步对直启不是多余的。
        if !name.is_empty() {
            loop {
                tokio::time::sleep(process::POLL_INTERVAL).await;
                if !sessions.read().await.contains_key(&sid) {
                    return;
                }
                if !process::is_running(&name) {
                    break;
                }
            }
        }

        tracing::info!("session {sid}: {name} exited");
        sessions.write().await.remove(&sid);
        // `None`(观测会话)时它什么都不做:那是用户自己的 prefix,关不得。
        #[cfg(unix)]
        super::gamescope::close_wine(prefix.as_deref()).await;
        let _ = events.send(SessionEvent {
            session_id: sid,
            game_id,
            kind: SessionKind::Ended,
        });
    });
}

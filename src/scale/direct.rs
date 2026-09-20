//! 无 gamescope 的两类会话:**观测**(只盯进程,什么都不启动)与**直接启动**
//! (不走缩放,把游戏跑起来)。两个引擎共用 —— 会话登记、watcher 与 `Ended`
//! 事件只写一遍,而退出后自动上传恰恰挂在 `Ended` 上,两端的直启都必须发它。
//!
//! 观测会话由 `daemon::watch` 的后台循环发起(**进程已经确认在跑**),所以这里没有
//! "等它出现"那一段 —— 理由与踩过的坑写在 `spawn_watch_task` 的文档里。
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

/// 这一局跟着**谁**。两种钥匙各有各的用处,所以都留着:
///
/// * [`Follow::Name`] —— 名字。能存进配置、下一局还认得出来,是"自动追踪"那条路;
/// * [`Follow::Pid`] —— 用户从运行中的进程里挑的那**一个**。名字给不了这种精确:
///   两款游戏都叫 `Game.exe` 时,只有 pid 说得清现在跑的是哪一款。代价是它只对这一次
///   运行有意义(进程一退,号迟早会被内核发给别人),所以它不进配置。
#[derive(Debug, Clone)]
pub(super) enum Follow {
    Name(String),
    Pid { pid: i32, name: String },
}

impl Follow {
    /// 它还在跑吗?
    fn alive(&self) -> bool {
        match self {
            Follow::Name(name) => process::is_running(name),
            Follow::Pid { pid, .. } => process::pid_alive(*pid),
        }
    }

    /// 日志里怎么称呼这一局。
    fn label(&self) -> String {
        match self {
            Follow::Name(name) => name.clone(),
            Follow::Pid { pid, name } if name.is_empty() => format!("pid {pid}"),
            Follow::Pid { pid, name } => format!("{name}（pid {pid}）"),
        }
    }

    /// 会话表里记的进程名(界面拿它显示"在跟谁")。
    fn display_name(&self) -> String {
        match self {
            Follow::Name(name) => name.clone(),
            Follow::Pid { pid, name } if name.is_empty() => format!("pid {pid}"),
            Follow::Pid { name, .. } => name.clone(),
        }
    }
}

/// Track a game the user starts themselves: kotori launches nothing, the
/// session simply follows one process for as long as it runs.
///
/// **调用方必须是"已经看到它在跑"的那一方**(`daemon::watch` 按名字,或用户从
/// 运行中的进程里挑了一个 pid)。
pub(super) async fn start_watch_session(
    sessions: &Sessions,
    events: &Events,
    spec: &LaunchSpec<'_>,
) -> Result<ScaleSession, ScaleError> {
    let name = spec.process_name.unwrap_or_default().trim().to_string();
    let follow = match spec.follow_pid {
        Some(pid) => Follow::Pid { pid, name },
        None if !name.is_empty() => Follow::Name(name),
        None => {
            return Err(ScaleError::ProtocolError(
                "观测会话必须知道要盯哪个进程名(或者一个 pid)".to_string(),
            ));
        }
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
        process_name: Some(follow.display_name()),
        follow_pid: spec.follow_pid,
        output_size: (0, 0),
        // Nothing was launched, so there is no prefix of ours to close.
        wine_prefix: None,
        watch_only: true,
        direct: false,
    };

    register(sessions, events, &session).await;
    tracing::info!(
        "watching for process {} (session {})",
        follow.label(),
        session.session_id
    );

    spawn_watch_task(
        sessions.clone(),
        events.clone(),
        session.session_id.clone(),
        session.game_id.clone(),
        follow,
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
        follow_pid: None,
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
        Follow::Name(name.clone().unwrap_or_default()),
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
/// `child` 是直启时我们 spawn 的进程(`None` = 观测)。两条路的开头不一样,但都很短:
///
/// * 直启:启动时已经确认那个进程活着(300ms 检测),所以只等它退出 —— 退出时名字
///   还在跑说明是启动器交接(真游戏还在),继续跟到走光。这与 `gamescope.rs` 的收尾
///   同构:那边 child 是 gamescope。
/// * 观测:**我们开始跟它的时候,它已经确定在跑了** —— 会话是 `daemon::watch` 的
///   后台循环在进程表里看到它之后才开的(用户从前得手点一次「启动」,那一按也只是
///   让 daemon 开始盯)。所以这里同样没有"等它出现"这一步。
///
/// ⚠ 这个函数曾经有过一段"先等名字出现(上限 300 秒)"的开头,两条路共用,而它错在
/// 两个地方:直启用它会每次结束都空等满 300 秒,并且走那条"进程始终没出现,放弃"
/// 的分支 —— 那条分支**不发 `Ended`**,而 `Ended` 是退出后自动上传的唯一触发器
/// (Windows 上没有 gamescope,每个游戏都走直启,整条链因此是断的)。**没有"等出现"
/// 就没有这条分支**,这也是它现在敢删掉的理由:观测会话的发起人已经确认过它活着。
///
/// `prefix` 是这一局用的 wine prefix(观测恒为 `None`):游戏都走光之后关掉
/// wine 的那摊,否则 `winedevice.exe` 会把一次注销拖成 90 秒。
fn spawn_watch_task(
    sessions: Sessions,
    events: Events,
    sid: String,
    game_id: Option<String>,
    follow: Follow,
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
        }

        // 两条路共用的收尾:等它走光。启动器交接时,我们启动的那个先退,真游戏顶着
        // 这个名字还在跑 —— 所以这一步对直启不是多余的。按 pid 跟的那种不做交接
        // 推断:用户指的就是那一个进程,它没了这一局就结束。
        // 名字是空的 = 这一局连"跟谁"都没有(直启且 exe 名都取不出来),直接收尾。
        let watchable = match &follow {
            Follow::Name(name) => !name.is_empty(),
            Follow::Pid { .. } => true,
        };
        if watchable {
            loop {
                tokio::time::sleep(process::POLL_INTERVAL).await;
                if !sessions.read().await.contains_key(&sid) {
                    return;
                }
                if !follow.alive() {
                    break;
                }
            }
        }

        tracing::info!("session {sid}: {} exited", follow.label());
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

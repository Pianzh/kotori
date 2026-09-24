use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::sync::broadcast;

use crate::config::{FALLBACK_OUTPUT_HEIGHT, FALLBACK_OUTPUT_WIDTH};
use crate::process;
use crate::util::executor::find_binary;

use super::teardown::{
    GAME_GONE_GRACE, GAME_POLL, TEARDOWN_GRACE, TEARDOWN_POLL, close_wine_unshared,
    kill_session_now, pid_alive, stuck_in_teardown, terminate_session,
};

use super::x11::{GamescopeDisplay, Settings};
use super::{
    LaunchSpec, ScaleAction, ScaleEngine, ScaleError, ScaleSession, ScaleStatus, SessionEvent,
    SessionKind, build_gamescope_args, profile_ratio,
};

/// How many lifecycle events may queue up before slow subscribers miss one.
///
/// Only the daemon subscribes, and it handles each event in a spawned task, so
/// this never has to be deep.
const EVENT_BUFFER: usize = 64;

/// Should one runtime action touch this session?
///
/// Split out from [`GamescopeScaleEngine::apply_action`] for one reason: the rule is
/// worth a test, and an id is cheaper to build in a test than a whole session.
/// Naming a session means *that* one; naming none means all of them (a caller with no
/// session in hand). A name that matches nothing touches nothing — it must never fall
/// back to "then everything".
fn wants(session_id: &str, only: Option<&str>) -> bool {
    only.is_none_or(|wanted| wanted == session_id)
}

/// The gamescope backend: runs a game inside a nested gamescope.
///
/// Named for what it drives rather than for a desktop, because that is what it is:
/// the same engine runs on niri and on KDE (where the window control in
/// `crate::desktop::kde` joins in).
///
/// `sessions` only stores session metadata; the live `Child` is owned by a
/// spawned watcher task that reaps it on exit, preventing zombie accumulation.
pub struct GamescopeScaleEngine {
    gamescope_path: String,
    wine_path: String,
    sessions: Arc<RwLock<HashMap<String, ScaleSession>>>,
    /// Lifecycle notifications for whoever wants to react to them (save sync).
    events: broadcast::Sender<SessionEvent>,
}

impl GamescopeScaleEngine {
    pub fn new() -> Self {
        let gamescope_path = find_binary("gamescope")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "gamescope".to_string());
        let wine_path = find_binary("wine")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "wine".to_string());

        Self {
            gamescope_path,
            wine_path,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            events: broadcast::channel(EVENT_BUFFER).0,
        }
    }

    /// Announce something, ignoring the case where nobody is listening.
    fn announce(&self, session: &ScaleSession, kind: SessionKind) {
        // A send error only means "no subscribers", which is fine.
        let _ = self.events.send(SessionEvent {
            session_id: session.session_id.clone(),
            game_id: session.game_id.clone(),
            kind,
        });
    }

    /// Wrap a game command so it runs inside gamescope via `gamescope <args> -- wine game.exe`.
    fn compose_command(&self, spec: &LaunchSpec<'_>, screen: (u32, u32)) -> Vec<String> {
        let mut game_cmd = vec![self.wine_path.clone(), spec.exe.to_string()];
        game_cmd.extend(spec.args.iter().cloned());

        build_gamescope_args(spec.profile, screen, &game_cmd)
    }

    /// The settings gamescope is actually running with, as far as anyone can tell.
    ///
    /// gamescope never writes those properties back, so what
    /// [`GamescopeDisplay::read`] returns is *our* last command — true as long as
    /// nothing else changed the filter. Falling back to the profile keeps a fresh
    /// session honest: it was launched with exactly those arguments.
    pub async fn live_settings(&self, session: &ScaleSession) -> Option<Settings> {
        // 自由参数模式下档案里那个算法**没有**发给 gamescope:回退到它等于报一个
        // 从来没存在过的状态。`None` 才是实话 —— "这一局不归我们管"。
        if session.profile.free_form() {
            return None;
        }
        let pid = session.gamescope_pid?;
        let display = GamescopeDisplay::discover(pid).ok().flatten()?;
        display
            .read()
            .ok()
            .flatten()
            .or_else(|| Some(Settings::for_algorithm(&session.profile.algorithm)))
    }

    /// Run one runtime scaling action against the live gamescopes.
    ///
    /// `only` narrows it to one session, and every caller that hands over a
    /// `session_id` means exactly that: two games open must not both get rescaled
    /// because the request happened to mention one. `None` = every live session, for
    /// a caller with no session in hand. Watch-only sessions are skipped either way —
    /// kotori launched nothing there, so there is no gamescope of ours to talk to.
    ///
    /// Filter actions and window actions live in different places: the filter is a
    /// property on gamescope's own Xwayland, the window belongs to the compositor.
    pub async fn apply_action(&self, action: ScaleAction, only: Option<&str>) -> ActionOutcome {
        let sessions: Vec<ScaleSession> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|session| wants(&session.session_id, only))
            .cloned()
            .collect();
        let mut outcome = ActionOutcome::default();
        for session in sessions {
            let Some(pid) = session.gamescope_pid else {
                continue;
            };
            // 直接启动的会话没有 gamescope:滤镜是 gamescope 的 X 属性,窗口尺寸
            // 是合成器的事,这里什么都没有。如实说"没动"。
            if session.direct {
                outcome.failed.push((
                    session.session_id.clone(),
                    "这一局是直接启动的，没有 gamescope 可调".to_string(),
                ));
                continue;
            }
            // 用户自己写了 gamescope 参数:运行时动作往 gamescope 的 Xwayland 属性里
            // 写滤镜与缩放,会当场盖掉他写下的 `-F`/`-S` —— 那正是他明确关掉的东西
            // (见 `ScaleProfile::free_form`)。如实说"没动",而不是假装成功。
            if session.profile.free_form() {
                outcome.failed.push((
                    session.session_id.clone(),
                    "这一局用的是自定义 gamescope 参数，kotori 不动它的缩放设置".to_string(),
                ));
                continue;
            }
            let result = if action.is_filter() {
                self.apply_filter(pid, &session, action)
                    .map(|it| describe(&it))
            } else {
                self.apply_window(pid, &session, action).await
            };
            match result {
                Ok(detail) => outcome.applied.push(AppliedAction {
                    session_id: session.session_id.clone(),
                    detail,
                }),
                Err(err) => outcome
                    .failed
                    .push((session.session_id.clone(), err.to_string())),
            }
        }
        outcome
    }

    /// Filter, scaler and sharpness: properties gamescope watches on its Xwayland.
    fn apply_filter(
        &self,
        pid: u32,
        session: &ScaleSession,
        action: ScaleAction,
    ) -> Result<Settings, ApplyError> {
        let Some(gs) = GamescopeDisplay::discover(pid)? else {
            return Err(ApplyError::NoGamescope);
        };
        let current = gs
            .read()?
            .unwrap_or_else(|| Settings::for_algorithm(&session.profile.algorithm));
        let next = current.applied(action);
        gs.apply(next)?;
        tracing::info!(
            "session {} ({}): {} → {}",
            session.session_id,
            gs.display(),
            action.id(),
            describe(&next)
        );
        Ok(next)
    }

    /// Window size and fullscreen: the compositor's business, KDE only.
    ///
    /// An action changes the *scale ratio*, which is what the user is looking at:
    /// the game keeps rendering at its own resolution, and gamescope's output — the
    /// window — grows, so the upscaling ratio grows with it. The ratio is the
    /// session's own record; the geometry itself belongs to KWin.
    async fn apply_window(
        &self,
        pid: u32,
        session: &ScaleSession,
        action: ScaleAction,
    ) -> Result<String, ApplyError> {
        if !crate::desktop::is_kde() {
            return Err(ApplyError::Unsupported(
                "窗口缩放与全屏暂只在 KDE 上实现（niri 是平铺合成器，窗口尺寸由布局决定）"
                    .to_string(),
            ));
        }

        if action == ScaleAction::ToggleFullscreen {
            crate::desktop::kde::toggle_fullscreen(pid).await?;
            return Ok("全屏开关（方向由 KWin 决定）".to_string());
        }

        // One key: to the configured ratio, or back to 1:1. Everything else on
        // this path is a ladder step, which the CLI still offers for anything
        // between the two.
        let ratio = if action == ScaleAction::ToggleScale {
            super::toggled_ratio(
                session.runtime_ratio,
                super::toggle_target(&session.profile, session.output_size),
            )
        } else {
            let index = super::ladder_index_for(session.runtime_ratio);
            super::SCALE_LADDER[match action {
                ScaleAction::ScaleUp => super::ladder_step(index, true),
                ScaleAction::ScaleDown => super::ladder_step(index, false),
                _ => 0, // ResetScale, and anything else that reaches here
            }]
        };
        // 游戏自己渲染的尺寸就是窗口尺寸的基准;档案里没写时用 gamescope 的默认值 ——
        // 那一局启动时没发 -w/-h,它画的就是这个尺寸。
        let (internal_width, internal_height) = session.profile.internal_size();
        let width = (internal_width as f32 * ratio).round() as u32;
        let height = (internal_height as f32 * ratio).round() as u32;
        crate::desktop::kde::resize_window(pid, width, height).await?;

        // Remember where we are, so the next press steps from here. The window is
        // the compositor's and its geometry cannot be read back without a D-Bus
        // service of our own, so this is the record.
        if let Some(stored) = self.sessions.write().await.get_mut(&session.session_id) {
            stored.runtime_ratio = ratio;
        }
        tracing::info!(
            "session {}: {} → 输出 {width}x{height}（{ratio}×）",
            session.session_id,
            action.id()
        );
        Ok(format!("输出 {width}x{height}（{ratio}×）"))
    }
}

/// The output a launch should size its window for, as far as kotori can tell before
/// the window exists.
///
/// The window is placed by the compositor, not by us, so this is the *primary*
/// output rather than the one the game will land on — good enough for the initial
/// size, and the only thing available this early.
///
/// ⚠ 窗口落到**另一块**屏时,启动之后**还没有**按那块屏再修一次:`desktop::kde` 的
/// `resize_window` 只在运行时动作里被调用,启动路径没接线(2026-09-13 核对过)。
/// 所以这句话从前写着"之后可以按实际输出修正"是不准确的 —— 要修得先让 KWin 脚本
/// 把结果回传(见 HANDOVER §5 里 `resize_window` 那条)。
fn screen_size() -> (u32, u32) {
    crate::display::primary_resolution_or((FALLBACK_OUTPUT_WIDTH, FALLBACK_OUTPUT_HEIGHT))
}

/// Human-readable summary of a filter setting, for logs and RPC answers.
fn describe(settings: &Settings) -> String {
    format!(
        "filter {:?} / scaler {:?} / 锐度 {}",
        settings.filter, settings.scaler, settings.sharpness
    )
}

/// Why an action could not be applied.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("gamescope 已经不在了（或者它的 Xwayland 还没起来）")]
    NoGamescope,
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    X11(#[from] super::x11::X11Error),
    #[error(transparent)]
    Kde(#[from] crate::desktop::kde::KdeError),
}

/// One session whose runtime settings changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedAction {
    pub session_id: String,
    /// What actually changed, in the words the CLI prints.
    pub detail: String,
}

/// What a runtime action did, session by session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Sessions that were actually changed.
    pub applied: Vec<AppliedAction>,
    /// Sessions that could not be reached, and why.
    pub failed: Vec<(String, String)>,
}

impl Default for GamescopeScaleEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Wait for the game's process to show up before watching for it to leave.
///
/// Without this, "not running" would be indistinguishable from "has not started
/// yet", and a slow wine prefix would have its session killed from under it.
///
/// Returns `false` when the session is already over, gamescope is gone, or the
/// process never appeared: a game that never starts is not this detector's
/// business — gamescope exiting on its own ends the session the ordinary way.
async fn game_shows_up(
    sessions: &Arc<RwLock<HashMap<String, ScaleSession>>>,
    sid: &str,
    name: &str,
    root: i32,
) -> bool {
    let deadline = tokio::time::Instant::now() + process::APPEAR_TIMEOUT;
    loop {
        if !sessions.read().await.contains_key(sid) {
            return false;
        }
        if process::is_running(name) {
            tracing::info!("session {sid}: 开始盯着游戏进程 {name}（它退出即收尾）");
            return true;
        }
        if !pid_alive(root) {
            return false;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "session {sid}: 一直没看到进程 {name}，不再盯它\
                 （gamescope 自己退出时仍会正常收尾）"
            );
            return false;
        }
        tokio::time::sleep(process::POLL_INTERVAL).await;
    }
}

#[async_trait::async_trait]
impl ScaleEngine for GamescopeScaleEngine {
    async fn start_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
        if spec.watch_only {
            return super::direct::start_watch_session(&self.sessions, &self.events, spec).await;
        }
        // 用户选了"不走缩放直接启动":不套 gamescope,但会话登记与退出后的
        // 自动上传照旧(见 `direct`)。
        if spec.direct_launch {
            return super::direct::start_direct_session(&self.sessions, &self.events, spec).await;
        }

        if find_binary("gamescope").is_none() {
            return Err(ScaleError::GamescopeNotFound);
        }
        if find_binary("wine").is_none() {
            return Err(ScaleError::WineNotFound);
        }

        let screen = screen_size();
        let cmd = self.compose_command(spec, screen);
        tracing::debug!("gamescope command: {} {:?}", self.gamescope_path, cmd);

        // Run gamescope (and thus wine) with the *game root* as CWD: many
        // visual novels resolve assets/config relative to it.
        let cwd = if spec.game_dir.as_os_str().is_empty() {
            Path::new(spec.exe)
                .parent()
                .unwrap_or_else(|| Path::new("."))
        } else {
            spec.game_dir
        };

        // Spawn gamescope as process-group leader so we can kill the whole
        // tree later.
        let mut command = Command::new(&self.gamescope_path);
        command.args(&cmd).current_dir(cwd).process_group(0);

        // Run under the resolved prefix instead of whatever wine defaults to.
        if let Some(prefix) = spec.wine_prefix {
            command.env("WINEPREFIX", prefix);
        }

        // gamescope enables its Vulkan "gamescope WSI" layer for the game
        // (`setenv("ENABLE_GAMESCOPE_WSI", "1", 0)` — the trailing 0 means "only if
        // not already set"), and that layer's explicit-sync path is what makes
        // gamescope die on NVIDIA in nested Wayland mode: measured here as
        // `vkImportSemaphoreFdKHR failed` followed by SIGSEGV about seven seconds
        // into the game, with `ENABLE_GAMESCOPE_WSI=0` surviving (ValveSoftware/
        // gamescope#1662). Disabling it costs one copy per frame (DXVK presents
        // through Xwayland rather than straight into gamescope) — it does *not*
        // replace DXVK, which is the only D3D path on the ARM target. A user who
        // wants to try the layer anyway only has to export ENABLE_GAMESCOPE_WSI=1.
        if std::env::var_os("ENABLE_GAMESCOPE_WSI").is_none() {
            command.env("ENABLE_GAMESCOPE_WSI", "0");
        }

        let mut child = command
            .spawn()
            .map_err(|e| ScaleError::GamescopeStartFailed(format!("{cmd:?}: {e}")))?;

        // Wait a short moment to catch immediate startup failure.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let exited = child
            .try_wait()
            .map_err(|e| ScaleError::GamescopeStartFailed(format!("try_wait: {e}")))?;

        if let Some(status) = exited {
            return Err(ScaleError::GamescopeStartFailed(format!(
                "gamescope exited immediately with {status}\ncommand: {} {:?}",
                self.gamescope_path, cmd
            )));
        }

        let pgid = child.id().unwrap_or(0);

        // The process that *is* the game, as opposed to wine's plumbing around it.
        //
        // Wine hands the exe's process a rewritten `argv[0]` (the Windows path), so
        // the exe's own file name is what matches it — unless the game config names
        // the process, which is what a launcher wrapper needs.
        let game_process = spec
            .process_name
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                Path::new(spec.exe)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .filter(|name| !name.trim().is_empty())
            });

        // 把这个 prefix 记进数据目录:万一这一局之后我们没机会收尾(daemon 被 SIGKILL、
        // 被另一个实例顶掉),下一个 daemon 至少能在关机时把它关干净(见 `wine_prefixes`)。
        if let Some(prefix) = spec.wine_prefix {
            crate::wine_prefixes::record(prefix);
        }

        let session = ScaleSession {
            session_id: uuid::Uuid::new_v4().to_string(),
            game_id: Some(spec.game_id.to_string()),
            gamescope_pid: Some(pgid),
            profile: spec.profile.clone(),
            runtime_ratio: profile_ratio(spec.profile, screen),
            started_at: std::time::Instant::now(),
            process_group: Some(pgid),
            process_name: spec.process_name.map(str::to_string),
            exe_path: Some(PathBuf::from(spec.exe)),
            output_size: spec.profile.output_size_for(screen),
            wine_prefix: spec.wine_prefix.map(Path::to_path_buf),
            watch_only: false,
            direct: false,
        };

        self.sessions
            .write()
            .await
            .insert(session.session_id.clone(), session.clone());
        self.announce(&session, SessionKind::Started);

        // Watcher task: reap the child and drop the session when the game
        // exits, so we never accumulate zombies and stale sessions.
        let sessions = self.sessions.clone();
        let events = self.events.clone();
        let sid = session.session_id.clone();
        let game_id = session.game_id.clone();
        let watched = session.process_name.clone();
        let wine_prefix = session.wine_prefix.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // `code()` is `None` when a signal killed the process, which is exactly
            // the case worth seeing: gamescope aborts on its way out here, and
            // "Ok(None)" alone says nothing about that.
            match &status {
                Ok(status) => tracing::info!(
                    "session {sid} gamescope exited: code={:?} signal={:?}",
                    status.code(),
                    std::os::unix::process::ExitStatusExt::signal(status)
                ),
                Err(err) => tracing::warn!("session {sid}: 等 gamescope 结束出错：{err}"),
            }

            // Launcher games: the processes we started may hand off to the real
            // game and exit first. Stay alive while that process still runs, so
            // clients do not treat a running game as finished.
            if let Some(name) = &watched {
                process::wait_until_gone(name).await;
            }

            // Everything of the game's is gone by now; wine's server is the last
            // thing to close, or it leaves a `winedevice.exe` behind.
            close_wine_unshared(&sessions, wine_prefix.as_deref(), Some(sid.as_str())).await;

            sessions.write().await.remove(&sid);
            let _ = events.send(SessionEvent {
                session_id: sid,
                game_id,
                kind: SessionKind::Ended,
            });
        });

        // Exit watchdog: see `stuck_in_teardown`. Without it, closing a game's
        // window leaves a frozen window, a KDE "not responding" prompt — and, worse
        // than either, a session that never ends, so the daemon never fires
        // `Ended` and the saves the game just wrote are never uploaded.
        let watchdog = pgid as i32;
        let watchdog_sid = session.session_id.clone();
        let watchdog_sessions = self.sessions.clone();
        let watchdog_prefix = session.wine_prefix.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TEARDOWN_POLL).await;
                if !watchdog_sessions.read().await.contains_key(&watchdog_sid) {
                    return; // stopped by hand; `stop_session` owns that teardown
                }
                if !pid_alive(watchdog) {
                    return; // gone: the ordinary path, nothing to clean up
                }
                if !stuck_in_teardown(watchdog) {
                    continue;
                }

                // The grace the user allowed, for a shutdown that finishes on its
                // own. Looking a second time is what makes this safe: a state seen
                // once is not a stuck session.
                tokio::time::sleep(TEARDOWN_GRACE).await;
                if !stuck_in_teardown(watchdog) {
                    return;
                }

                tracing::warn!(
                    "session {watchdog_sid}: gamescope 卡在等子进程（wine 的 winedevice.exe \
                     不理会 SIGTERM），kotori 直接收尾整组进程与它的子进程树"
                );
                // 没有任何宽限:走到这里时 gamescope 已经卡在 `wait4`,再等它也不会自己动。
                // 但**必须连子进程树一起杀** —— `winedevice.exe` 住在自己的进程组里,
                // 只 `kill(-pgid)` 会把它留下,而那正是让整机卡 90 秒的东西。
                kill_session_now(watchdog);
                // 还留着的那一份就用 wine 自己的服务器收:`wineserver -k` 之后
                // `winedevice.exe` 才会真的消失(实测)。
                close_wine_unshared(
                    &watchdog_sessions,
                    watchdog_prefix.as_deref(),
                    Some(watchdog_sid.as_str()),
                )
                .await;
                return;
            }
        });

        // A second way for a session to end: the game leaving, as opposed to
        // gamescope getting wedged on its way out.
        //
        // Measured (2026-09-12): a game closed from its **own** menu leaves
        // gamescope running with nothing to composite — its main thread stays in the
        // Wayland poll loop, so `stuck_in_teardown` is false and the watchdog above
        // never fires. What *is* gone is the game, so that is what gets watched.
        // Without this the session never ends: no `Ended`, and the saves the game
        // just wrote are never uploaded.
        if let Some(name) = game_process {
            let detector_sessions = self.sessions.clone();
            let detector_sid = session.session_id.clone();
            let detector_prefix = session.wine_prefix.clone();
            let root = watchdog;
            tokio::spawn(async move {
                if !game_shows_up(&detector_sessions, &detector_sid, &name, root).await {
                    return;
                }
                loop {
                    tokio::time::sleep(GAME_POLL).await;
                    if !detector_sessions.read().await.contains_key(&detector_sid) {
                        return; // stopped by hand; `stop_session` owns that teardown
                    }
                    if !pid_alive(root) {
                        return; // gamescope is gone: the ordinary path
                    }
                    if process::is_running(&name) {
                        continue;
                    }

                    // Gone once is not gone: wine starts its processes in stages.
                    tokio::time::sleep(GAME_GONE_GRACE).await;
                    if process::is_running(&name) {
                        continue;
                    }

                    // A launcher exits before the game it started does, and that
                    // game is still sitting in this session's process tree.
                    if !process::live_game_processes(root).is_empty() {
                        continue;
                    }

                    tracing::warn!(
                        "session {detector_sid}: 游戏进程 {name} 已经退出，但 gamescope 还活着\
                         （游戏内部退出／启动器交接），kotori 收尾整组进程"
                    );
                    terminate_session(root).await;
                    close_wine_unshared(
                        &detector_sessions,
                        detector_prefix.as_deref(),
                        Some(detector_sid.as_str()),
                    )
                    .await;
                    return;
                }
            });
        }

        Ok(session)
    }

    async fn stop_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        let exists = self.sessions.read().await.contains_key(&session.session_id);
        if !exists {
            return Err(ScaleError::SessionNotFound(session.session_id.clone()));
        }

        // A watch-only session owns no process: dropping it *is* the stop.
        let Some(pgid) = session.process_group.map(|pgid| pgid as i32) else {
            self.sessions.write().await.remove(&session.session_id);
            tracing::info!("stopped watch-only session {}", session.session_id);
            return Ok(());
        };

        tracing::info!("stopping session {} (pgid {})", session.session_id, pgid);

        // SIGTERM the group *and* its tree, then SIGKILL what is left. The tree
        // matters here too: `winedevice.exe` sits in a process group of its own.
        terminate_session(pgid).await;

        // ...and the tree is still not the whole story: the same `winedevice.exe`
        // outlives the kill, so wine's server is closed for this prefix as well.
        close_wine_unshared(
            &self.sessions,
            session.wine_prefix.as_deref(),
            Some(session.session_id.as_str()),
        )
        .await;

        Ok(())
    }

    async fn wait_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        tracing::info!("Waiting for session {} to exit...", session.session_id);
        loop {
            if self.get_session(&session.session_id).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Ok(())
    }

    async fn get_session(&self, session_id: &str) -> Option<ScaleSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    async fn list_sessions(&self) -> Vec<ScaleSession> {
        self.sessions.read().await.values().cloned().collect()
    }

    fn subscribe(&self) -> Option<broadcast::Receiver<SessionEvent>> {
        Some(self.events.subscribe())
    }

    async fn get_status(&self, session: &ScaleSession) -> Result<ScaleStatus, ScaleError> {
        Ok(ScaleStatus {
            fsr_enabled: matches!(
                session.profile.algorithm,
                crate::config::ScaleAlgorithm::Fsr { .. }
            ),
            current_sharpness: session.profile.algorithm.sharpness().unwrap_or(0),
            integer_scaling: matches!(
                session.profile.algorithm,
                crate::config::ScaleAlgorithm::Integer
            ),
            current_resolution: session.output_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::wants;

    #[test]
    fn an_action_named_for_one_session_ignores_the_others() {
        assert!(wants("a", Some("a")));
        assert!(!wants("b", Some("a")), "两个游戏同时跑时不能一起改");
        // 没有指名 = 全部(留给手上没有会话的调用方)。
        assert!(wants("a", None) && wants("b", None));
        // 名字对不上就什么都不做,而不是退化成"那就全都改"。
        assert!(!wants("a", Some("ghost")));
    }
}

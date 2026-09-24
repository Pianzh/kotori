//! gamescope 后端那个类型本身:`GamescopeScaleEngine` 与它的非 trait 方法。
//!
//! 从 `gamescope.rs` 拆出来 —— 那边连着测试一起数越过了 500 行的硬线(AGENTS.md),
//! 而它偏偏是"只能拆实现"的那一个(测试只有十几行)。**trait 实现留在
//! `gamescope.rs`**:一个 trait impl 的所有方法必须在同一个块里,拆不动。
//!
//! ⚠ 可见性:`new`/`announce`/`compose_command`/`apply_filter`/`apply_window` 与那几个
//! 字段都放宽到 `pub(super)` —— 它们原来只有这个文件看得见,现在兄弟模块
//! (`gamescope.rs` 的 trait impl)要用。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use crate::util::executor::find_binary;

use super::{ActionOutcome, AppliedAction, ApplyError, describe, wants};
use crate::scale::x11::{GamescopeDisplay, Settings};
use crate::scale::{
    LaunchSpec, ScaleAction, ScaleSession, SessionEvent, SessionKind, build_gamescope_args,
};

/// How many lifecycle events may queue up before slow subscribers miss one.
///
/// Only the daemon subscribes, and it handles each event in a spawned task, so
/// this never has to be deep.
const EVENT_BUFFER: usize = 64;
/// The gamescope backend: runs a game inside a nested gamescope.
///
/// Named for what it drives rather than for a desktop, because that is what it is:
/// the same engine runs on niri and on KDE (where the window control in
/// `crate::desktop::kde` joins in).
///
/// `sessions` only stores session metadata; the live `Child` is owned by a
/// spawned watcher task that reaps it on exit, preventing zombie accumulation.
pub struct GamescopeScaleEngine {
    pub(super) gamescope_path: String,
    pub(super) wine_path: String,
    pub(super) sessions: Arc<RwLock<HashMap<String, ScaleSession>>>,
    /// Lifecycle notifications for whoever wants to react to them (save sync).
    pub(super) events: broadcast::Sender<SessionEvent>,
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
    pub(super) fn announce(&self, session: &ScaleSession, kind: SessionKind) {
        // A send error only means "no subscribers", which is fine.
        let _ = self.events.send(SessionEvent {
            session_id: session.session_id.clone(),
            game_id: session.game_id.clone(),
            kind,
        });
    }

    /// Wrap a game command so it runs inside gamescope via `gamescope <args> -- wine game.exe`.
    pub(super) fn compose_command(&self, spec: &LaunchSpec<'_>, screen: (u32, u32)) -> Vec<String> {
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
    pub(super) fn apply_filter(
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
    pub(super) async fn apply_window(
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
            crate::scale::toggled_ratio(
                session.runtime_ratio,
                crate::scale::toggle_target(&session.profile, session.output_size),
            )
        } else {
            let index = crate::scale::ladder_index_for(session.runtime_ratio);
            crate::scale::SCALE_LADDER[match action {
                ScaleAction::ScaleUp => crate::scale::ladder_step(index, true),
                ScaleAction::ScaleDown => crate::scale::ladder_step(index, false),
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

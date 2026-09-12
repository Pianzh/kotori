use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::sync::broadcast;

use crate::process;
use crate::util::executor::find_binary;

use super::x11::{GamescopeDisplay, Settings};
use super::{
    LaunchSpec, ScaleAction, ScaleEngine, ScaleError, ScaleSession, ScaleStatus, SessionEvent,
    SessionKind, build_gamescope_args,
};

/// How many lifecycle events may queue up before slow subscribers miss one.
///
/// Only the daemon subscribes, and it handles each event in a spawned task, so
/// this never has to be deep.
const EVENT_BUFFER: usize = 64;

/// Niri (Wayland) backend: runs gamescope as a nested compositor.
///
/// `sessions` only stores session metadata; the live `Child` is owned by a
/// spawned watcher task that reaps it on exit, preventing zombie accumulation.
pub struct NiriScaleEngine {
    gamescope_path: String,
    wine_path: String,
    sessions: Arc<RwLock<HashMap<String, ScaleSession>>>,
    /// Lifecycle notifications for whoever wants to react to them (save sync).
    events: broadcast::Sender<SessionEvent>,
}

impl NiriScaleEngine {
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

    /// Track a game the user starts themselves: kotori launches nothing, the
    /// session simply follows `process_name` for as long as it runs.
    async fn start_watch_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
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
            started_at: std::time::Instant::now(),
            process_group: None,
            process_name: Some(name.to_string()),
            watch_only: true,
        };

        self.sessions
            .write()
            .await
            .insert(session.session_id.clone(), session.clone());
        tracing::info!(
            "watching for process {name} (session {})",
            session.session_id
        );
        self.announce(&session, SessionKind::Started);

        let sessions = self.sessions.clone();
        let events = self.events.clone();
        let sid = session.session_id.clone();
        let game_id = session.game_id.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + process::APPEAR_TIMEOUT;

            // Wait for the game to show up — unless the session is stopped.
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
                    // Deliberately no `Ended`: the game never ran, so a client
                    // must not treat this as "a game finished, sync it".
                    return;
                }
                tokio::time::sleep(process::POLL_INTERVAL).await;
            }

            tracing::info!("session {sid}: {name} is running");
            loop {
                tokio::time::sleep(process::POLL_INTERVAL).await;
                if !sessions.read().await.contains_key(&sid) {
                    return;
                }
                if !process::is_running(&name) {
                    break;
                }
            }

            tracing::info!("session {sid}: {name} exited");
            sessions.write().await.remove(&sid);
            let _ = events.send(SessionEvent {
                session_id: sid,
                game_id,
                kind: SessionKind::Ended,
            });
        });

        Ok(session)
    }

    /// Wrap a game command so it runs inside gamescope via `gamescope <args> -- wine game.exe`.
    fn compose_command(&self, spec: &LaunchSpec<'_>) -> Vec<String> {
        let mut game_cmd = vec![self.wine_path.clone(), spec.exe.to_string()];
        game_cmd.extend(spec.args.iter().cloned());

        build_gamescope_args(spec.profile, &game_cmd)
    }

    /// The settings gamescope is actually running with, as far as anyone can tell.
    ///
    /// gamescope never writes those properties back, so what
    /// [`GamescopeDisplay::read`] returns is *our* last command — true as long as
    /// nothing else changed the filter. Falling back to the profile keeps a fresh
    /// session honest: it was launched with exactly those arguments.
    pub async fn live_settings(&self, session: &ScaleSession) -> Option<Settings> {
        let pid = session.gamescope_pid?;
        let display = GamescopeDisplay::discover(pid).ok().flatten()?;
        display
            .read()
            .ok()
            .flatten()
            .or_else(|| Some(Settings::for_algorithm(&session.profile.algorithm)))
    }

    /// Run one runtime scaling action against every live gamescope.
    ///
    /// All of them, not just the focused one: the hotkey is global, and a user with
    /// two games open is rare enough that "both changed" (which the answer says)
    /// beats "the wrong one changed". Watch-only sessions are skipped — kotori
    /// launched nothing there, so there is no gamescope of ours to talk to.
    pub async fn apply_action(&self, action: ScaleAction) -> ActionOutcome {
        let sessions: Vec<ScaleSession> = self.sessions.read().await.values().cloned().collect();
        let mut outcome = ActionOutcome::default();
        for session in sessions {
            let Some(pid) = session.gamescope_pid else {
                continue;
            };
            match self.apply_to(pid, &session, action) {
                Ok(Some(settings)) => outcome.applied.push(AppliedAction {
                    session_id: session.session_id.clone(),
                    settings,
                }),
                Ok(None) => {}
                Err(err) => outcome
                    .failed
                    .push((session.session_id.clone(), err.to_string())),
            }
        }
        outcome
    }

    fn apply_to(
        &self,
        pid: u32,
        session: &ScaleSession,
        action: ScaleAction,
    ) -> Result<Option<Settings>, super::x11::X11Error> {
        let Some(gs) = GamescopeDisplay::discover(pid)? else {
            // gamescope is gone, or its Xwayland has not come up yet.
            return Ok(None);
        };
        let current = gs
            .read()?
            .unwrap_or_else(|| Settings::for_algorithm(&session.profile.algorithm));
        let next = current.applied(action);
        gs.apply(next)?;
        tracing::info!(
            "session {} ({}): {} → filter {:?} / scaler {:?} / 锐度 {}",
            session.session_id,
            gs.display(),
            action.id(),
            next.filter,
            next.scaler,
            next.sharpness
        );
        Ok(Some(next))
    }
}

/// One session whose runtime settings changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedAction {
    pub session_id: String,
    pub settings: Settings,
}

/// What a runtime action did, session by session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Sessions that were actually changed.
    pub applied: Vec<AppliedAction>,
    /// Sessions that could not be reached, and why.
    pub failed: Vec<(String, String)>,
}

impl Default for NiriScaleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ScaleEngine for NiriScaleEngine {
    async fn start_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
        if spec.watch_only {
            return self.start_watch_session(spec).await;
        }

        if find_binary("gamescope").is_none() {
            return Err(ScaleError::GamescopeNotFound);
        }
        if find_binary("wine").is_none() {
            return Err(ScaleError::WineNotFound);
        }

        let cmd = self.compose_command(spec);
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
        let session = ScaleSession {
            session_id: uuid::Uuid::new_v4().to_string(),
            game_id: Some(spec.game_id.to_string()),
            gamescope_pid: Some(pgid),
            profile: spec.profile.clone(),
            started_at: std::time::Instant::now(),
            process_group: Some(pgid),
            process_name: spec.process_name.map(str::to_string),
            watch_only: false,
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
        tokio::spawn(async move {
            let status = child.wait().await;
            tracing::info!(
                "session {sid} gamescope exited: {:?}",
                status.map(|s| s.code())
            );

            // Launcher games: the processes we started may hand off to the real
            // game and exit first. Stay alive while that process still runs, so
            // clients do not treat a running game as finished.
            if let Some(name) = &watched {
                process::wait_until_gone(name).await;
            }

            sessions.write().await.remove(&sid);
            let _ = events.send(SessionEvent {
                session_id: sid,
                game_id,
                kind: SessionKind::Ended,
            });
        });

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

        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }

        // Allow a few seconds for graceful shutdown before SIGKILL.
        for _ in 0..30 {
            if !pid_alive(pgid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if pid_alive(pgid) {
            tracing::warn!(
                "session {} did not exit, sending SIGKILL",
                session.session_id
            );
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }

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
            current_resolution: (session.profile.output_width, session.profile.output_height),
        })
    }
}

/// Check whether a process (by pid) still exists.
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM means the process exists but belongs to another user.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

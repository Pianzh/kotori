use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::RwLock;

use crate::util::executor::find_binary;

use super::{LaunchSpec, ScaleEngine, ScaleError, ScaleSession, ScaleStatus, build_gamescope_args};

/// Niri (Wayland) backend: runs gamescope as a nested compositor.
///
/// `sessions` only stores session metadata; the live `Child` is owned by a
/// spawned watcher task that reaps it on exit, preventing zombie accumulation.
pub struct NiriScaleEngine {
    gamescope_path: String,
    wine_path: String,
    sessions: Arc<RwLock<HashMap<String, ScaleSession>>>,
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
        }
    }

    /// Wrap a game command so it runs inside gamescope via `gamescope <args> -- wine game.exe`.
    fn compose_command(&self, spec: &LaunchSpec<'_>) -> Vec<String> {
        let mut game_cmd = vec![self.wine_path.clone(), spec.exe.to_string()];
        game_cmd.extend(spec.args.iter().cloned());

        build_gamescope_args(spec.profile, &game_cmd)
    }
}

impl Default for NiriScaleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ScaleEngine for NiriScaleEngine {
    async fn start_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
        if find_binary("gamescope").is_none() {
            return Err(ScaleError::GamescopeNotFound);
        }

        // Watch-only games are started by the user, never by us.
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
            gamescope_pid: pgid,
            profile: spec.profile.clone(),
            started_at: std::time::Instant::now(),
            process_group: pgid,
        };

        self.sessions
            .write()
            .await
            .insert(session.session_id.clone(), session.clone());

        // Watcher task: reap the child and drop the session when the game
        // exits, so we never accumulate zombies and stale sessions.
        let sessions = self.sessions.clone();
        let sid = session.session_id.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            tracing::info!("session {sid} exited: {:?}", status.map(|s| s.code()));
            sessions.write().await.remove(&sid);
        });

        Ok(session)
    }

    async fn stop_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        let exists = self.sessions.read().await.contains_key(&session.session_id);
        if !exists {
            return Err(ScaleError::SessionNotFound(session.session_id.clone()));
        }

        let pgid = session.process_group as i32;
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

    async fn toggle_fsr(&self, _session: &ScaleSession) -> Result<(), ScaleError> {
        Err(ScaleError::ProtocolError(
            "gamescope 不支持外部运行时控制，请聚焦游戏窗口后按 Super+U 切换 FSR".to_string(),
        ))
    }

    async fn adjust_sharpness(
        &self,
        _session: &ScaleSession,
        _delta: i32,
    ) -> Result<(), ScaleError> {
        Err(ScaleError::ProtocolError(
            "gamescope 不支持外部运行时控制，请用 Super+I / Super+O 调整锐度".to_string(),
        ))
    }

    async fn toggle_integer(&self, _session: &ScaleSession) -> Result<(), ScaleError> {
        Err(ScaleError::ProtocolError(
            "gamescope 不支持外部运行时控制，请用 Super+N 切换最近邻滤波".to_string(),
        ))
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

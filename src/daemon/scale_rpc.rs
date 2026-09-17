//! The `scale.*` requests: scaling a game that is already running.
//!
//! Every one of them lands in [`Daemon::run_action`] — the CLI and the GUI both
//! go through here, so they cannot disagree about what an action means.

use serde_json::{Value, json};

use super::*;

impl Daemon {
    pub(super) async fn rpc_scale_status(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        let status = self
            .engine
            .get_status(&session)
            .await
            .map_err(|e| e.to_string())?;
        let value = serde_json::to_value(status).map_err(|e| e.to_string())?;

        // `status` describes the profile the game was launched with. What gamescope
        // is running *now* is readable too — it is our own last command, kept on the
        // root window of its Xwayland — so report both and let the caller spot when
        // they have drifted apart (gamescope's own shortcuts can change it too).
        //
        // ⚠ 这一段**只存在于有 gamescope 的机器上**,不是"为了编译而 cfg":Windows 上
        // 没有一个"外部工具此刻在用什么滤镜"的可读状态(Magpie 那套只暴露窗口属性,
        // 见 PLATFORMS.md §2.3),所以这里本来就没有东西可报告。`live_settings` 也
        // 因此不必进 trait —— 它是 gamescope 后端专有的读法。
        #[cfg(unix)]
        let value = {
            let mut value = value;
            if let Some(object) = value.as_object_mut()
                && let Some(live) = self.engine.live_settings(&session).await
            {
                object.insert(
                    "live".to_string(),
                    json!({
                        "filter": format!("{:?}", live.filter),
                        "scaler": format!("{:?}", live.scaler),
                        "sharpness": live.sharpness,
                    }),
                );
            }
            value
        };

        Ok(value)
    }

    /// Runtime scaling changes go straight at gamescope: every action is written to
    /// the properties it watches on its own Xwayland (see `crate::scale::x11`).
    ///
    /// Nothing but a live session is needed — no consent dialog, no key binding —
    /// so this works from the CLI and the GUI. What it does need is a live session:
    /// without one there is nothing to rescale, and the answer says so.
    pub(super) async fn rpc_scale_toggle_fsr(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.run_action(&session, crate::scale::ScaleAction::ToggleFsr)
            .await
    }

    /// Nearest-neighbour is the runtime counterpart of integer scaling: both stop
    /// the filter from inventing pixels.
    pub(super) async fn rpc_scale_toggle_integer(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.run_action(&session, crate::scale::ScaleAction::ToggleNearest)
            .await
    }

    /// Sharpness moves one step per call, so a bigger delta means stepping more
    /// than once (capped at gamescope's 0..20 range).
    ///
    /// `delta` is in kotori's own scale, where larger is *sharper* — the inversion
    /// into gamescope's softness number lives in
    /// [`crate::scale::ScaleAction::for_sharpness_delta`].
    pub(super) async fn rpc_scale_adjust_sharpness(
        &self,
        session_id: &str,
        delta: i32,
    ) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        let action = crate::scale::ScaleAction::for_sharpness_delta(delta)
            .ok_or_else(|| "锐度步长不能为 0".to_string())?;
        let steps = delta.unsigned_abs().min(20);
        for _ in 0..steps {
            self.run_action(&session, action).await?;
        }
        Ok(json!({ "success": true, "steps": steps }))
    }

    /// Run any action by id.
    ///
    /// What `kotori scale up|down|reset|fullscreen` uses; the ids come from
    /// [`crate::scale::ScaleAction`], so a caller and this handler cannot drift apart.
    pub(super) async fn rpc_scale_action(
        &self,
        session_id: &str,
        action: crate::scale::ScaleAction,
    ) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.run_action(&session, action).await
    }

    /// Run one action against the live sessions and describe what happened.
    ///
    /// "Nothing to do" is an error the caller can act on ("start a game first"),
    /// while a partial success reports both halves — silence about the session that
    /// did *not* change is how a user ends up running the command twice.
    pub(super) async fn run_action(
        &self,
        session: &ScaleSession,
        action: crate::scale::ScaleAction,
    ) -> Result<Value, String> {
        // 只作用在这一局上:调用方给了 session_id 就是想改它,两个游戏同时跑时
        // 不能因为"形状像全局"就把另一个也改了。
        let outcome = self
            .engine
            .apply_action(action, Some(&session.session_id))
            .await;
        if outcome.applied.is_empty() {
            let detail = if outcome.failed.is_empty() {
                if session.gamescope_pid.is_none() {
                    format!(
                        "会话 {} 只是观测（watch_only），kotori 没有它的 gamescope 可调",
                        session.session_id
                    )
                } else {
                    "没有正在运行的游戏".to_string()
                }
            } else {
                outcome
                    .failed
                    .iter()
                    .map(|(session, err)| format!("{session}: {err}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            return Err(format!("{} 没有生效：{detail}", action.id()));
        }
        Ok(json!({
            "success": true,
            "action": action.id(),
            "sessions": outcome
                .applied
                .iter()
                .map(|a| json!({ "session": a.session_id, "detail": a.detail }))
                .collect::<Vec<_>>(),
            "failed": outcome
                .failed
                .iter()
                .map(|(session, err)| json!({ "session": session, "error": err }))
                .collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn lookup_session(&self, session_id: &str) -> Result<ScaleSession, String> {
        self.engine
            .get_session(session_id)
            .await
            .ok_or_else(|| format!("session not found: {session_id}"))
    }
}

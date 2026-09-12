//! The `scale.*` requests: scaling a game that is already running.
//!
//! These are the actions the hotkeys fire — [`Daemon::run_action`] is the one
//! place both arrive at — so a key and the CLI cannot disagree about what an
//! action means.

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
        let mut value = serde_json::to_value(status).map_err(|e| e.to_string())?;
        // `status` describes the profile the game was launched with. What gamescope
        // is running *now* is readable too — it is our own last command, kept on the
        // root window of its Xwayland — so report both and let the caller spot when
        // they have drifted apart (a hotkey, or gamescope's own shortcuts).
        if let Some(object) = value.as_object_mut() {
            object.insert("hotkeys_ready".to_string(), json!(crate::hotkeys::ready()));
            if let Some(live) = self.engine.live_settings(&session).await {
                object.insert(
                    "live".to_string(),
                    json!({
                        "filter": format!("{:?}", live.filter),
                        "scaler": format!("{:?}", live.scaler),
                        "sharpness": live.sharpness,
                    }),
                );
            }
        }
        Ok(value)
    }

    /// Runtime scaling changes go straight at gamescope: every action is written to
    /// the properties it watches on its own Xwayland (see `crate::scale::x11`).
    ///
    /// No hotkeys and no consent dialog are involved in *doing* it — the portal
    /// only supplies the trigger — so this works from the CLI and the GUI even when
    /// the user never bound a key. What it does need is a live session: without one
    /// there is nothing to rescale, and the answer says so.
    pub(super) async fn rpc_scale_toggle_fsr(&self, session_id: &str) -> Result<Value, String> {
        self.lookup_session(session_id).await?;
        self.run_action(crate::scale::ScaleAction::ToggleFsr).await
    }

    /// Nearest-neighbour is the runtime counterpart of integer scaling: both stop
    /// the filter from inventing pixels.
    pub(super) async fn rpc_scale_toggle_integer(&self, session_id: &str) -> Result<Value, String> {
        self.lookup_session(session_id).await?;
        self.run_action(crate::scale::ScaleAction::ToggleNearest)
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
        self.lookup_session(session_id).await?;
        let action = crate::scale::ScaleAction::for_sharpness_delta(delta)
            .ok_or_else(|| "锐度步长不能为 0".to_string())?;
        let steps = delta.unsigned_abs().min(20);
        for _ in 0..steps {
            self.run_action(action).await?;
        }
        Ok(json!({ "success": true, "steps": steps }))
    }

    /// Run any registered action by id.
    ///
    /// What `kotori scale up|down|reset|fullscreen` uses, and the same ids a hotkey
    /// arrives with — so a key binding and the CLI can never drift apart.
    pub(super) async fn rpc_scale_action(
        &self,
        session_id: &str,
        action: crate::scale::ScaleAction,
    ) -> Result<Value, String> {
        self.lookup_session(session_id).await?;
        self.run_action(action).await
    }

    /// Run one action against the live sessions and describe what happened.
    ///
    /// "Nothing to do" is an error the caller can act on ("start a game first"),
    /// while a partial success reports both halves — silence about the session that
    /// did *not* change is how a user ends up pressing a key twice.
    pub(super) async fn run_action(
        &self,
        action: crate::scale::ScaleAction,
    ) -> Result<Value, String> {
        let outcome = self.engine.apply_action(action).await;
        if outcome.applied.is_empty() {
            let detail = if outcome.failed.is_empty() {
                "没有正在运行的游戏".to_string()
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

    /// Ask the desktop portal for the runtime-scaling hotkeys.
    ///
    /// Launching a game already does this (ADR-015); this is the way in when
    /// nothing of ours is running yet — the CLI's `kotori scale hotkeys`, and
    /// what the real-machine check uses to separate "registration failed" from
    /// "injection failed".
    ///
    /// Registration pops a consent dialog, so the answer describes what was
    /// *started*; poll `daemon.status` for the outcome.
    pub(super) fn rpc_scale_hotkeys(&self) -> Value {
        let started = crate::hotkeys::request_once(self.hotkey_sink());
        let status = crate::hotkeys::status();
        json!({
            "started_now": started,
            "requested": status.requested,
            "ready": status.ready,
            "error": status.error,
            "unbound": status.unbound,
            "assign_hint": status.assign_hint,
        })
    }
}

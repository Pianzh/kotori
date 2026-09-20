//! `handle_request`:`daemon/mod.rs` 那张方法名 → RPC 的**唯一**分派表。
//!
//! 从 `mod.rs` 整体搬来(纯移动,只加了个 `pub(super)`):那边已经贴着 500 行硬线,
//! 而这一轮要往里加自动追踪与配置来源切换两件事。分派表本身跟守护进程的生命周期
//! 没有关系 —— 它只做"名字对不对、参数够不够、转给哪个 `rpc_*`"。

use serde_json::{Value, json};

use super::protocol::{GamePatch, NewGame, Reply, param_str, respond, rpc_err, rpc_ok};
use super::*;

impl Daemon {
    pub(super) async fn handle_request(&self, raw: &str) -> Reply {
        let req: protocol::rpc::Request = match serde_json::from_str(raw) {
            Ok(req) => req,
            Err(e) => return rpc_err(Value::Null, -32700, format!("parse error: {e}")),
        };
        if req.jsonrpc != "2.0" {
            return rpc_err(
                req.id,
                -32600,
                format!("unsupported jsonrpc version: {:?}", req.jsonrpc),
            );
        }

        let id = req.id.clone();
        match req.method.as_str() {
            "daemon.status" => respond(id, self.rpc_status().await),
            "daemon.shutdown" => Reply {
                body: rpc_ok(id, json!({ "success": true })).body,
                shutdown: true,
            },
            "config.reload" => respond(id, self.rpc_reload_config().await),
            "wine.status" => respond(id, self.rpc_wine_status().await),
            "env.report" => respond(id, self.rpc_env_report().await),
            "wine.set_prefix" => {
                let value = match req.params.as_ref().and_then(|p| p.get("prefix")) {
                    Some(v) => v.clone(),
                    None => return rpc_err(id, -32602, "缺少参数: prefix".to_string()),
                };
                respond(id, self.rpc_set_wine_prefix(value).await)
            }

            "game.list" => respond(id, self.rpc_game_list().await),
            "game.remove" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_game_remove(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.create" => {
                match serde_json::from_value::<NewGame>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(new_game) => respond(id, self.rpc_game_create(new_game).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "game.update" => {
                let game_id = match param_str(&req.params, "id") {
                    Ok(v) => v.to_string(),
                    Err(e) => return rpc_err(id, -32602, e),
                };
                let mut patch_fields = req.params.clone().unwrap_or_default();
                patch_fields.remove("id");
                if patch_fields.is_empty() {
                    return rpc_err(
                        id,
                        -32602,
                        "game.update 需要至少一个要修改的字段".to_string(),
                    );
                }
                match serde_json::from_value::<GamePatch>(Value::Object(patch_fields)) {
                    Ok(patch) => respond(id, self.rpc_game_update(&game_id, patch).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "game.launch" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_game_launch(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.wait" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_game_wait(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.stop" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_game_stop(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.get_status" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_status(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.toggle_fsr" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_toggle_fsr(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.toggle_integer" => match param_str(&req.params, "session_id") {
                Ok(sid) => respond(id, self.rpc_scale_toggle_integer(sid).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.adjust_sharpness" => match param_str(&req.params, "session_id") {
                Ok(sid) => {
                    let delta = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("delta"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    respond(id, self.rpc_scale_adjust_sharpness(sid, delta).await)
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            "scale.action" => match (
                param_str(&req.params, "session_id"),
                param_str(&req.params, "action"),
            ) {
                (Ok(sid), Ok(action)) => match crate::scale::ScaleAction::from_id(action) {
                    Some(action) => respond(id, self.rpc_scale_action(sid, action).await),
                    None => rpc_err(id, -32602, format!("未知的缩放动作：{action}")),
                },
                (Err(e), _) | (_, Err(e)) => rpc_err(id, -32602, e),
            },
            "sync.status" => respond(id, self.rpc_sync_status().await),
            "sync.set_settings" => {
                match serde_json::from_value::<sync_rpc::SettingsPatch>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(patch) => respond(id, self.rpc_sync_set_settings(patch).await),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_credentials" => {
                match serde_json::from_value::<sync_rpc::Credentials>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(credentials) => respond(id, self.rpc_sync_set_credentials(credentials)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_kopia_password" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_set_kopia_password(password)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.unlock" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_unlock(password)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.set_master_password" => {
                match serde_json::from_value::<sync_rpc::Password>(Value::Object(
                    req.params.clone().unwrap_or_default(),
                )) {
                    Ok(password) => respond(id, self.rpc_sync_set_master_password(password)),
                    Err(e) => rpc_err(id, -32602, format!("参数无效: {e}")),
                }
            }
            "sync.clear_master_password" => respond(id, self.rpc_sync_clear_master_password()),
            "sync.lock" => respond(id, self.rpc_sync_lock()),
            "sync.test" => respond(id, self.rpc_sync_test().await),
            "sync.now" => {
                let game_id = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                respond(id, self.rpc_sync_now(game_id.as_deref()).await)
            }
            "sync.versions" => match param_str(&req.params, "id") {
                Ok(game_id) => respond(id, self.rpc_sync_versions(game_id).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "sync.restore" => match param_str(&req.params, "id") {
                Ok(game_id) => {
                    let version = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    respond(id, self.rpc_sync_restore(game_id, version.as_deref()).await)
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            other => rpc_err(id, -32601, format!("method not found: {other}")),
        }
    }
}

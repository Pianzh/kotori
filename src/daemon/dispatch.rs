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
            "config.set_source" => {
                let portable = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("portable"))
                    .and_then(|v| v.as_bool());
                match portable {
                    Some(portable) => respond(id, self.rpc_config_set_source(portable).await),
                    None => rpc_err(id, -32602, "缺少参数: portable(真假)".to_string()),
                }
            }
            "process.list" => respond(id, self.rpc_process_list().await),
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
            "mount.infer" => match param_str(&req.params, "path") {
                Ok(path) => respond(id, self.rpc_mount_infer(Path::new(path)).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "game.add" => match param_str(&req.params, "directory") {
                Ok(directory) => respond(id, self.rpc_game_add(Path::new(directory)).await),
                Err(e) => rpc_err(id, -32602, e),
            },
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
                Ok(game_id) => {
                    // `selfcheck: true` = 这个客户端答得上"启动前那一问"
                    // （见 `rpc_game_launch`）。
                    let selfcheck = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("selfcheck"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    respond(id, self.rpc_game_launch(game_id, selfcheck).await)
                }
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
            "sync.cloud_games" => respond(id, self.rpc_sync_cloud_games().await),
            // 云端现在有哪些游戏：读**本机缓存**（一个桶一份的索引），不读身份卡。
            // `refresh: true` 是「云端存档」页那颗刷新按钮（强制联网）；不传就只在缓存过了
            // 一小时、或者本地还没有缓存时才去云端（见 `sync_rpc::index::cloud_index_view`）。
            "sync.cloud_list" => {
                let refresh = req
                    .params
                    .as_ref()
                    .and_then(|params| params.get("refresh"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                respond(id, self.rpc_sync_cloud_list(refresh).await)
            }
            // 云端某一款的版本：参数是**云端落点**（`sync.cloud_games` 给的 id），不是
            // 本机 id —— 云端有而本机没有的游戏也要能列出它的版本。
            "sync.cloud_versions" => match param_str(&req.params, "key") {
                Ok(cloud_key) => respond(id, self.rpc_sync_cloud_versions(cloud_key).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            // 添加游戏时那一问：这个 exe 在云端是哪一款（读索引，只读不写）。
            // 参数是**本机路径**，指纹由 daemon 自己算（不让客户端递）。
            "sync.match" => match param_str(&req.params, "exe") {
                Ok(exe) => respond(id, self.rpc_sync_match(std::path::Path::new(exe)).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            // 启动前自检的对话框：用户选了哪一项（`ok` / `off` / `pair`）。
            "sync.resolve" => match param_str(&req.params, "id") {
                Ok(game_id) => {
                    let choice = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("choice"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cloud_id = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("cloud_id"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let cloud_key = req
                        .params
                        .as_ref()
                        .and_then(|p| p.get("cloud_key"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    respond(
                        id,
                        self.rpc_sync_resolve(
                            game_id,
                            &choice,
                            cloud_id.as_deref(),
                            cloud_key.as_deref(),
                        )
                        .await,
                    )
                }
                Err(e) => rpc_err(id, -32602, e),
            },
            "sync.pairing" => respond(id, self.rpc_sync_pairing().await),
            "sync.pair" => match (
                param_str(&req.params, "id"),
                param_str(&req.params, "cloud_key"),
                param_str(&req.params, "cloud_id"),
            ) {
                (Ok(local), Ok(key), Ok(cloud)) => {
                    respond(id, self.rpc_sync_pair(local, key, cloud).await)
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => rpc_err(id, -32602, e),
            },
            "sync.reject" => match (
                param_str(&req.params, "id"),
                param_str(&req.params, "cloud_id"),
            ) {
                (Ok(local), Ok(cloud)) => respond(id, self.rpc_sync_reject(local, cloud).await),
                (Err(e), _) | (_, Err(e)) => rpc_err(id, -32602, e),
            },
            // 删云端某一版 / 这一款的所有存档 / 整条词条。三者都按**云端落点**收参数：
            // 云端有、本机没有的游戏也要能清（与 `sync.cloud_versions` 同一把尺子）。
            "sync.delete_version" => match (
                param_str(&req.params, "key"),
                param_str(&req.params, "version"),
            ) {
                (Ok(key), Ok(version)) => {
                    respond(id, self.rpc_sync_delete_version(key, version).await)
                }
                (Err(e), _) | (_, Err(e)) => rpc_err(id, -32602, e),
            },
            "sync.delete_versions" => match param_str(&req.params, "key") {
                Ok(key) => respond(id, self.rpc_sync_delete_versions(key).await),
                Err(e) => rpc_err(id, -32602, e),
            },
            "sync.delete_identity" => match param_str(&req.params, "key") {
                Ok(key) => respond(id, self.rpc_sync_delete_identity(key).await),
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

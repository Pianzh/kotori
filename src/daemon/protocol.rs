//! The wire: the shapes of a request and a reply, and the helpers that build
//! them.
//!
//! JSON-RPC 2.0, one request per line over the daemon's Unix socket. [`rpc`] is
//! the client half; everything above it is what the daemon needs to answer.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Reply {
    pub(super) body: String,
    /// Set by `daemon.shutdown`; the caller signals after flushing the reply.
    pub(super) shutdown: bool,
}

pub(super) fn rpc_ok(id: Value, result: Value) -> Reply {
    Reply {
        body: json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        shutdown: false,
    }
}

pub(super) fn rpc_err(id: Value, code: i32, message: impl Into<String>) -> Reply {
    Reply {
        body: json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message.into() }
        })
        .to_string(),
        shutdown: false,
    }
}

pub(super) fn respond(id: Value, result: Result<Value, String>) -> Reply {
    match result {
        Ok(value) => rpc_ok(id, value),
        Err(message) => rpc_err(id, -32000, message),
    }
}

/// Required string parameter, or a JSON-RPC `invalid params` message.
pub(super) fn param_str<'a>(
    params: &'a Option<serde_json::Map<String, Value>>,
    key: &str,
) -> Result<&'a str, String> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing parameter: {key}"))
}

/// Fields a client may patch on an existing game. Absent keys are left alone;
/// `null` clears an optional field.
///
/// ⚠ `deny_unknown_fields` 是**故意**的:没有它的时候,调用方多包一层(或者把键名
/// 拼错一个字母)会被静默丢掉,却仍然拿到 `success: true` —— 调用方以为改了配置,
/// 其实一个字节都没动(真踩过)。现在这种包法直接报"参数无效"。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GamePatch {
    pub(super) name: Option<String>,
    pub(super) game_dir: Option<PathBuf>,
    pub(super) exe_path: Option<PathBuf>,
    pub(super) launch_args: Option<Vec<String>>,
    pub(super) save_paths: Option<Vec<crate::config::SavePath>>,
    #[serde(default, deserialize_with = "double_option")]
    pub(super) wine_prefix: Option<Option<PathBuf>>,
    /// 自动追踪。旧名字 `watch_only` 仍然收得下(见 `GameConfig::auto_watch`)——
    /// 界面上那只开关改叫「自动追踪」,而配置里曾经叫这个名字。
    #[serde(alias = "watch_only")]
    pub(super) auto_watch: Option<bool>,
    /// 这一款的云同步开关（每款一个，见 `GameConfig::sync_enabled`）。
    pub(super) sync_enabled: Option<bool>,
    pub(super) direct_launch: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub(super) process_name: Option<Option<String>>,
    /// 挂载引用（盘号 + 磁盘内相对目录）。用户 2026-09-25 定的口径：
    /// **键不出现 = 别动这一栏；`null` = 不再用引用**（回到绝对路径）；给了值 =
    /// 按它定位。"用不用引用"这个开关就在这里，别的一律不动这一栏。
    #[serde(default, deserialize_with = "double_option")]
    pub(super) game_dir_mount: Option<Option<crate::mount::MountPath>>,
    #[serde(default, deserialize_with = "double_option")]
    pub(super) exe_mount: Option<Option<crate::mount::MountPath>>,
    pub(super) profile: Option<crate::config::ScaleProfile>,
}

/// Explicit input for the manual "add game" path.
#[derive(Debug, Deserialize)]
pub(super) struct NewGame {
    pub(super) name: String,
    pub(super) exe_path: PathBuf,
    #[serde(default)]
    pub(super) game_dir: Option<PathBuf>,
    /// 建条目时直接带上挂载引用：有引用就**不要求路径此刻存在**（盘可能插在别的
    /// 机器上、也可能还没插）—— 用户 2026-09-25："大不了就是报错打不开，这是正常的"。
    #[serde(default)]
    pub(super) game_dir_mount: Option<crate::mount::MountPath>,
    #[serde(default)]
    pub(super) exe_mount: Option<crate::mount::MountPath>,
}

/// Tell `null` apart from "key absent" for `Option<Option<T>>` fields.
pub(super) fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub mod rpc {
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    pub struct Request {
        pub jsonrpc: String,
        pub id: Value,
        pub method: String,
        #[serde(default)]
        pub params: Option<serde_json::Map<String, Value>>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `game.update` 的字段是**平铺**在 `params` 里的(不是嵌在 `patch` 下)。
    /// 所以最容易犯的错是"多包一层",它从前会被静默忽略 —— 这里把它钉住。
    #[test]
    fn a_patch_key_the_daemon_does_not_know_is_rejected() {
        let known = json!({ "name": "示例游戏", "auto_watch": true });
        assert!(serde_json::from_value::<GamePatch>(known).is_ok());

        // 旧名字走 alias,别在升级后被拒(存量客户端 / 手写脚本还在用它)。
        let old_name = json!({ "watch_only": true });
        assert!(serde_json::from_value::<GamePatch>(old_name).is_ok());

        let nested = json!({ "patch": { "name": "示例游戏" } });
        let error = serde_json::from_value::<GamePatch>(nested).unwrap_err();
        assert!(error.to_string().contains("patch"), "{error}");

        let typo = json!({ "profil": {} });
        let error = serde_json::from_value::<GamePatch>(typo).unwrap_err();
        assert!(error.to_string().contains("profil"), "{error}");
    }

    /// 「键不在 = 不动这个字段」这条语义不能被误伤。
    #[test]
    fn an_absent_key_leaves_its_field_alone() {
        let patch: GamePatch = serde_json::from_value(json!({ "auto_watch": true })).unwrap();
        assert_eq!(patch.auto_watch, Some(true));
        assert!(patch.name.is_none());
        assert!(patch.profile.is_none());
    }
}

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
#[derive(Debug, Default, Deserialize)]
pub(super) struct GamePatch {
    pub(super) name: Option<String>,
    pub(super) game_dir: Option<PathBuf>,
    pub(super) exe_path: Option<PathBuf>,
    pub(super) launch_args: Option<Vec<String>>,
    pub(super) save_paths: Option<Vec<crate::config::SavePath>>,
    #[serde(default, deserialize_with = "double_option")]
    pub(super) wine_prefix: Option<Option<PathBuf>>,
    pub(super) watch_only: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub(super) process_name: Option<Option<String>>,
    pub(super) profile: Option<crate::config::ScaleProfile>,
}

/// Explicit input for the manual "add game" path.
#[derive(Debug, Deserialize)]
pub(super) struct NewGame {
    pub(super) name: String,
    pub(super) exe_path: PathBuf,
    #[serde(default)]
    pub(super) game_dir: Option<PathBuf>,
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

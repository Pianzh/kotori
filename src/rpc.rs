//! JSON-RPC client for talking to the kotori daemon over a Unix socket.
//!
//! Shared by the GUI and the CLI so both go through the daemon (the daemon owns
//! all runtime state; nothing else may spawn games).

use std::path::Path;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Send a JSON-RPC request to the daemon and receive a single response.
///
/// `game.wait` legitimately blocks until the game exits, so this waits for the
/// response without a client-side timeout.
pub async fn call(
    socket_path: &Path,
    method: &str,
    params: Option<serde_json::Map<String, Value>>,
) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| format!("无法连接守护进程（是否已启动？）: {e}"))?;

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params.unwrap_or_default(),
    });

    stream
        .write_all(req.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;

    let (reader, _writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "守护进程无响应".to_string())?;

    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("响应解析失败: {e}"))?;

    if let Some(err) = resp.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(msg.to_string());
    }

    resp.get("result")
        .cloned()
        .ok_or_else(|| "响应缺少 result".to_string())
}

/// Convenience: build a params map from `(key, value)` pairs.
pub fn params(
    pairs: impl IntoIterator<Item = (&'static str, Value)>,
) -> serde_json::Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

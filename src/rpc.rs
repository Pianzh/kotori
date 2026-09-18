//! JSON-RPC client for talking to the kotori daemon.
//!
//! Shared by the GUI and the CLI so both go through the daemon (the daemon owns
//! all runtime state; nothing else may spawn games).
//!
//! 传输是本机的,形状由 [`crate::daemon::ipc`] 那一层决定:Linux 是 Unix socket,
//! Windows 是命名管道。这里只认"一个能连上的端点" —— 那个 `socket_path` 在
//! Windows 上装的是管道名。

use std::path::Path;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 客户端这一侧的连接。
#[cfg(unix)]
type IpcStream = tokio::net::UnixStream;
#[cfg(windows)]
type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// 连上守护进程的本机端点。
#[cfg(unix)]
async fn connect(socket_path: &Path) -> Result<IpcStream, String> {
    IpcStream::connect(socket_path)
        .await
        .map_err(|e| format!("无法连接守护进程（是否已启动？）: {e}"))
}

/// Windows 上没有 `UnixStream`。`NamedPipeClient::connect` 是同步的,但连一条
/// 本机管道是即时的,不值得为此再套一层 `spawn_blocking`。
#[cfg(windows)]
async fn connect(socket_path: &Path) -> Result<IpcStream, String> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let name = socket_path.as_os_str().to_string_lossy().into_owned();
    ClientOptions::new()
        .open(&*name)
        .map_err(|e| format!("无法连接守护进程（是否已启动？）: {e}"))
}

/// Send a JSON-RPC request to the daemon and receive a single response.
///
/// `game.wait` legitimately blocks until the game exits, so this waits for the
/// response without a client-side timeout.
pub async fn call(
    socket_path: &Path,
    method: &str,
    params: Option<serde_json::Map<String, Value>>,
) -> Result<Value, String> {
    let stream = connect(socket_path).await?;

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params.unwrap_or_default(),
    });

    // 先拆再写:两半都要。`tokio::io::split` 对两种流都成立,不像
    // `UnixStream::into_split` 只属于 Unix 那一种。
    let (reader, mut writer) = tokio::io::split(stream);
    writer
        .write_all(req.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())?;

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

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

/// Windows 上没有 `UnixStream`。`ClientOptions::open` 是同步的,但连一条本机管道是
/// 即时的,不值得为此再套一层 `spawn_blocking`。
///
/// 这里要重试的**只有** `ERROR_PIPE_BUSY`(231)。别的错误(最常见的是"系统找不到
/// 指定的文件",也就是守护进程根本没跑)必须立刻返回 —— 否则该报错的地方会先白等。
#[cfg(windows)]
async fn connect(socket_path: &Path) -> Result<IpcStream, String> {
    use tokio::net::windows::named_pipe::ClientOptions;

    /// 所有管道实例都在忙。
    const ERROR_PIPE_BUSY: i32 = 231;

    let name = socket_path.as_os_str().to_string_lossy().into_owned();
    let mut busy = None;

    // 服务端会预建下一个空闲实例(见 `daemon::ipc` 里 `Listener::pending` 的注释),
    // 所以"忙"只可能是极短的瞬间。留一点余量就行,不要无限等。
    for attempt in 0..20u32 {
        match ClientOptions::new().open(&*name) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                busy = Some(e);
                let backoff = if attempt < 4 { 2 } else { 15 };
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            }
            Err(e) => return Err(format!("无法连接守护进程（是否已启动？）: {e}")),
        }
    }

    Err(format!(
        "无法连接守护进程（管道的实例一直占着）: {}",
        busy.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// 端点那头有没有守护进程 —— 只探连接,不发请求。
///
/// 调用方靠它决定"这次是走 RPC 还是自己干"(`kotori add`,见 `cli::add_cli`):
/// 连不上**不是错误**,是"没有别人在替我写配置"。
pub async fn is_running(socket_path: &Path) -> bool {
    connect(socket_path).await.is_ok()
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    async fn response_error(body: &'static [u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "kotori-rpc-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut byte = [0u8; 1];
            while stream.read(&mut byte).unwrap_or(0) > 0 {
                if byte[0] == b'\n' {
                    break;
                }
            }
            stream.write_all(body).unwrap();
        });

        let result = call(&path, "daemon.status", None).await;
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
        result.unwrap_err()
    }

    #[tokio::test]
    async fn malformed_empty_and_error_responses_are_reported() {
        assert!(
            response_error(b"{not-json\n")
                .await
                .contains("响应解析失败")
        );
        assert_eq!(response_error(b"").await, "守护进程无响应");
        assert_eq!(
            response_error(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-1,\"message\":\"boom\"}}\n"
            )
            .await,
            "boom"
        );
        assert_eq!(
            response_error(b"{\"jsonrpc\":\"2.0\",\"id\":1}\n").await,
            "响应缺少 result"
        );
    }

    #[tokio::test]
    async fn a_missing_endpoint_is_reported_as_a_connection_error() {
        let path = std::env::temp_dir().join(format!(
            "kotori-rpc-missing-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let error = call(&path, "daemon.status", None).await.unwrap_err();
        assert!(error.contains("无法连接守护进程"), "{error}");
    }
}

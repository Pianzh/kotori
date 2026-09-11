//! End-to-end test of the daemon IPC contract.
//!
//! Spawns the real `kotori daemon` binary against a throw-away config/socket in
//! the temp dir (via `KOTORI_CONFIG` / `KOTORI_SOCKET`) and drives it over the
//! Unix socket, exactly like the GUI and CLI do.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    socket: PathBuf,
    log: PathBuf,
    child: Option<Child>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-e2e-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let config = dir.join("config.toml");
        let socket = dir.join("kotori.sock");
        let log = dir.join("daemon.log");

        // The config points at a *different* socket than KOTORI_SOCKET, which
        // proves the env override wins.
        let config_body = format!(
            r#"
[daemon]
socket_path = "{}"
log_level = "info"

[games.demo]
name = "Demo"
exe_path = "/games/demo/game.exe"
save_paths = ["/games/demo/save"]
created_at = "2026-01-01T00:00:00Z"

[games.demo.scale_profile]
name = "自定义"
internal_width = 1920
internal_height = 1080
output_width = 2560
output_height = 1440
framerate_limit = 60
force_fullscreen = false

[games.demo.scale_profile.algorithm.Nis]
sharpness = 4
"#,
            dir.join("wrong.sock").display()
        );
        std::fs::write(&config, config_body).unwrap();

        Self {
            dir,
            config,
            socket,
            log,
            child: None,
        }
    }

    fn start(&mut self) {
        let stdout = std::fs::File::create(&self.log).unwrap();
        let stderr = stdout.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_kotori"))
            .arg("daemon")
            .env("KOTORI_CONFIG", &self.config)
            .env("KOTORI_SOCKET", &self.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("failed to spawn kotori daemon");
        self.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon socket never became reachable\n{}", self.logs());
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket)
            .unwrap_or_else(|e| panic!("connect failed: {e}\n{}", self.logs()));
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        stream.write_all(request.to_string().as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad response {line:?}: {e}"))
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let child = self.child.as_mut().expect("not started");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match child.try_wait().unwrap() {
                Some(_) => return true,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        false
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn assert_is_error(response: &Value, code: i32) {
    let value: Value = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(
        value["error"]["code"], code,
        "expected error {code}, got {response}"
    );
}

#[test]
fn daemon_ipc_end_to_end() {
    let mut fixture = Fixture::new("ipc");
    fixture.start();

    assert!(
        fixture.socket.exists(),
        "KOTORI_SOCKET override should win over the config value"
    );
    assert!(
        !fixture.dir.join("wrong.sock").exists(),
        "config socket_path must be ignored when KOTORI_SOCKET is set"
    );

    // --- daemon.status -----------------------------------------------------
    let response = fixture.rpc("daemon.status", json!({}));
    let status = &response["result"];
    assert_eq!(status["running"], true, "{response}");
    assert_eq!(status["games"], 1);
    assert!(status["sessions"].as_array().unwrap().is_empty());

    // --- game.list returns the complete profile ----------------------------
    let response = fixture.rpc("game.list", json!({}));
    let game = &response["result"]["games"][0];
    assert_eq!(game["id"], "demo");
    assert_eq!(game["name"], "Demo");
    assert_eq!(game["scale_profile"]["name"], "自定义");
    // Regression: these used to be dropped, so saving from the UI wiped them.
    assert_eq!(game["scale_profile"]["algorithm"]["Nis"]["sharpness"], 4);
    assert_eq!(game["scale_profile"]["framerate_limit"], 60);
    assert_eq!(game["scale_profile"]["force_fullscreen"], false);
    assert_eq!(game["scale_profile"]["output_width"], 2560);
    assert_eq!(game["save_paths"][0], "/games/demo/save");

    // --- error paths -------------------------------------------------------
    assert_is_error(&fixture.rpc("does.not.exist", json!({})), -32601);
    let response = fixture.rpc("game.launch", json!({ "id": "nope" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Game not found"),
        "{response}"
    );
    let response = fixture.rpc("scale.get_status", json!({ "session_id": "ghost" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session not found"),
        "{response}"
    );

    // --- daemon.shutdown actually stops the daemon -------------------------
    let response = fixture.rpc("daemon.shutdown", json!({}));
    assert_eq!(response["result"]["success"], true, "{response}");

    assert!(
        fixture.wait_for_exit(Duration::from_secs(10)),
        "daemon did not exit after daemon.shutdown\n{}",
        fixture.logs()
    );

    // The socket file is cleaned up on the way out.
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !fixture.socket.exists(),
        "stale socket file left behind at {}",
        fixture.socket.display()
    );
}

#[test]
fn second_daemon_replaces_a_stale_socket_file() {
    let dir = std::env::temp_dir().join(format!("kotori-e2e-stale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stale = dir.join("stale.sock");
    std::fs::write(&stale, b"not a socket").unwrap();

    let mut fixture = Fixture::new("stale-restart");
    fixture.socket = stale.clone();
    fixture.start();

    let response = fixture.rpc("daemon.status", json!({}));
    assert_eq!(response["result"]["running"], true, "{response}");

    std::fs::remove_dir_all(&dir).ok();
}

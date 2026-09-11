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
    /// Extra directory prepended to the daemon's PATH (fake gamescope/wine).
    extra_path: Option<PathBuf>,
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
            extra_path: None,
        }
    }

    fn start(&mut self) {
        let stdout = std::fs::File::create(&self.log).unwrap();
        let stderr = stdout.try_clone().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_kotori"));
        command
            .arg("daemon")
            .env("KOTORI_CONFIG", &self.config)
            .env("KOTORI_SOCKET", &self.socket)
            // Keep display detection out of the test: the daemon must use this
            // value for new games regardless of the machine it runs on.
            .env("KOTORI_OUTPUT_RESOLUTION", "2560x1440")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if let Some(bin) = &self.extra_path {
            let existing = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{existing}", bin.display()));
        }
        let child = command.spawn().expect("failed to spawn kotori daemon");
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
    assert_eq!(game["save_paths"][0]["path"], "/games/demo/save");
    assert_eq!(game["save_paths"][0]["kind"], "absolute");

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

/// The daemon owns config mutation, so the GUI adds, repairs, renames and
/// removes entries over IPC instead of hand-editing the TOML.
#[test]
fn library_entries_are_managed_over_ipc() {
    let mut fixture = Fixture::new("library");
    fixture.start();

    // A game directory with an exe in it.
    let game_dir = fixture.dir.join("NewGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.chs.exe");
    std::fs::write(&exe, b"").unwrap();

    let game_count = |fixture: &Fixture| {
        fixture.rpc("game.list", json!({}))["result"]["games"]
            .as_array()
            .unwrap()
            .len()
    };

    // --- add ---------------------------------------------------------------
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "New Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "new-game", "{response}");
    assert_eq!(game_count(&fixture), 2);
    assert!(
        std::fs::read_to_string(&fixture.config)
            .unwrap()
            .contains("New Game"),
        "game.create must persist to disk"
    );

    // --- rename + repair the exe path --------------------------------------
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "new-game", "name": "Renamed Game", "exe_path": exe }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");
    let games = fixture.rpc("game.list", json!({}))["result"]["games"].clone();
    let renamed = games
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "new-game")
        .unwrap()
        .clone();
    assert_eq!(renamed["name"], "Renamed Game");
    assert_eq!(renamed["exe_path"], exe.to_string_lossy().as_ref());

    // A non-existent exe is rejected and leaves the stored value untouched.
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "new-game", "exe_path": "/nonexistent/game.exe" }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("不存在"),
        "{response}"
    );
    let games = fixture.rpc("game.list", json!({}))["result"]["games"].clone();
    let unchanged = games
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "new-game")
        .unwrap()
        .clone();
    assert_eq!(unchanged["exe_path"], exe.to_string_lossy().as_ref());

    // Updating without any field, or an unknown id, is an error.
    assert_is_error(
        &fixture.rpc("game.update", json!({ "id": "new-game" })),
        -32602,
    );
    assert_is_error(
        &fixture.rpc("game.update", json!({ "id": "ghost", "name": "x" })),
        -32000,
    );

    // --- remove ------------------------------------------------------------
    let response = fixture.rpc("game.remove", json!({ "id": "new-game" }));
    assert_eq!(response["result"]["success"], true, "{response}");
    assert_eq!(game_count(&fixture), 1);
    assert!(
        !std::fs::read_to_string(&fixture.config)
            .unwrap()
            .contains("Renamed Game"),
        "game.remove must persist to disk"
    );
    assert_is_error(
        &fixture.rpc("game.remove", json!({ "id": "new-game" })),
        -32000,
    );
}

/// The daemon is the only writer of the config, so the GUI persists a scale
/// profile with `game.update`; the patch must be validated and atomic.
#[test]
fn scale_profile_is_patched_and_validated_over_ipc() {
    let mut fixture = Fixture::new("profile");
    fixture.start();

    let demo = |fixture: &Fixture| {
        fixture.rpc("game.list", json!({}))["result"]["games"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == "demo")
            .unwrap()
            .clone()
    };

    // Sharpness 9 is representable but out of range: it must be clamped.
    let response = fixture.rpc(
        "game.update",
        json!({
            "id": "demo",
            "profile": {
                "name": "自定义",
                "algorithm": { "Fsr": { "sharpness": 9 } },
                "internal_width": 1920,
                "internal_height": 1080,
                "output_width": 2560,
                "output_height": 1440,
                "framerate_limit": 60,
                "force_fullscreen": false
            }
        }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    let game = demo(&fixture);
    assert_eq!(game["scale_profile"]["algorithm"]["Fsr"]["sharpness"], 5);
    assert_eq!(game["scale_profile"]["framerate_limit"], 60);
    assert_eq!(game["scale_profile"]["force_fullscreen"], false);
    assert_eq!(game["scale_profile"]["name"], "自定义");
    assert!(
        std::fs::read_to_string(&fixture.config)
            .unwrap()
            .contains("force_fullscreen = false"),
        "the profile patch must be persisted"
    );

    // An impossible resolution is rejected...
    let response = fixture.rpc(
        "game.update",
        json!({
            "id": "demo",
            "profile": {
                "name": "自定义",
                "algorithm": "Integer",
                "internal_width": 0,
                "internal_height": 1080,
                "output_width": 2560,
                "output_height": 1440,
                "force_fullscreen": false
            }
        }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("游戏分辨率宽"),
        "{response}"
    );

    // ...and leaves the previously stored profile untouched.
    let game = demo(&fixture);
    assert_eq!(game["scale_profile"]["framerate_limit"], 60);
    assert_eq!(game["scale_profile"]["algorithm"]["Fsr"]["sharpness"], 5);

    // A profile that is not a valid ScaleProfile is rejected too.
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "demo", "profile": { "algorithm": "NotAnAlgorithm" } }),
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("NotAnAlgorithm"),
        "{response}"
    );
}

/// Manual add (no scanning) plus the wine/save-path settings it needs.
#[test]
fn manual_add_and_wine_settings_over_ipc() {
    let mut fixture = Fixture::new("manual");
    fixture.start();

    // A game directory with an exe in it.
    let game_dir = fixture.dir.join("MyGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("MyGame.exe");
    std::fs::write(&exe, b"").unwrap();

    // --- game.create -------------------------------------------------------
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "My Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "my-game", "{response}");

    let game = |fixture: &Fixture, id: &str| {
        fixture.rpc("game.list", json!({}))["result"]["games"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .unwrap()
            .clone()
    };

    let created = game(&fixture, "my-game");
    assert_eq!(created["name"], "My Game");
    assert_eq!(created["game_dir"], game_dir.to_string_lossy().as_ref());
    assert_eq!(created["watch_only"], false);
    assert!(created["save_paths"].as_array().unwrap().is_empty());
    // New games inherit the detected output resolution (pinned by the fixture).
    assert_eq!(created["scale_profile"]["output_width"], 2560);
    assert_eq!(created["scale_profile"]["output_height"], 1440);

    // Duplicates, a missing exe and a bad game dir are refused.
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "My Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("已存在同名"),
        "{response}"
    );
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Ghost", "exe_path": "/nope/ghost.exe" }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("可执行文件不存在"),
        "{response}"
    );
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Bad Dir", "exe_path": exe, "game_dir": "/nope/nope" }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("游戏目录不存在"),
        "{response}"
    );

    // --- watch-only games are never launched by kotori ---------------------
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "my-game", "watch_only": true, "process_name": "MyGame.exe" }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");
    let updated = game(&fixture, "my-game");
    assert_eq!(updated["watch_only"], true);
    assert_eq!(updated["process_name"], "MyGame.exe");

    let response = fixture.rpc("game.launch", json!({ "id": "my-game" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("仅观测"),
        "{response}"
    );

    // --- save paths are validated by resolving them ------------------------
    let response = fixture.rpc(
        "game.update",
        json!({
            "id": "my-game",
            "save_paths": [
                "savedata",
                { "kind": "windows", "path": "%APPDATA%\\MyGame", "exclude": ["*.log"] }
            ]
        }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");
    let stored = game(&fixture, "my-game")["save_paths"].clone();
    assert_eq!(stored[0]["kind"], "relative");
    assert_eq!(stored[0]["path"], "savedata");
    assert_eq!(stored[1]["kind"], "windows");
    assert_eq!(stored[1]["exclude"][0], "*.log");

    // An unknown token is rejected, and the stored paths stay as they were.
    let response = fixture.rpc(
        "game.update",
        json!({
            "id": "my-game",
            "save_paths": [{ "kind": "windows", "path": "%NOPE%\\save" }]
        }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("%NOPE%"),
        "{response}"
    );
    assert_eq!(
        game(&fixture, "my-game")["save_paths"][1]["path"],
        "%APPDATA%\\MyGame"
    );

    // --- wine prefix: status, set, validate, clear -------------------------
    let status = fixture.rpc("wine.status", json!({}));
    assert_eq!(status["result"]["configured"], serde_json::Value::Null);
    assert!(status["result"]["detected"].is_array());

    let prefix = fixture.dir.join("prefix");
    std::fs::create_dir_all(prefix.join("drive_c/users/tester")).unwrap();
    let response = fixture.rpc("wine.set_prefix", json!({ "prefix": prefix }));
    assert_eq!(
        response["result"]["prefix"],
        prefix.to_string_lossy().as_ref()
    );

    // A directory that is not a prefix is refused.
    let response = fixture.rpc("wine.set_prefix", json!({ "prefix": fixture.dir }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("drive_c"),
        "{response}"
    );

    // The prefix is persisted, and a game inherits it.
    assert!(
        std::fs::read_to_string(&fixture.config)
            .unwrap()
            .contains("prefix = "),
        "wine prefix must be persisted"
    );
    let response = fixture.rpc("wine.status", json!({}));
    assert_eq!(
        response["result"]["configured"],
        prefix.to_string_lossy().as_ref()
    );

    // `null` (or an empty string) means "auto-detect" again.
    let response = fixture.rpc("wine.set_prefix", json!({ "prefix": null }));
    assert_eq!(response["result"]["prefix"], serde_json::Value::Null);
    assert_eq!(
        fixture.rpc("wine.status", json!({}))["result"]["configured"],
        serde_json::Value::Null
    );
}

/// Launch plumbing: the game root is the working directory, the resolved wine
/// prefix is exported, and the gamescope command carries the scaled profile.
///
/// Fake `gamescope` and `wine` scripts on PATH make this deterministic and
/// window-free: the fake gamescope records how it was invoked and exits.
#[test]
fn launch_builds_the_expected_gamescope_command() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new("launch");
    let bin = fixture.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let probe = fixture.dir.join("probe.txt");

    let script = format!(
        "#!/bin/sh\n{{ echo \"argv:$*\"; echo \"cwd:$(pwd)\"; echo \"WINEPREFIX:${{WINEPREFIX:-}}\"; }} >> '{}'\nexit 0\n",
        probe.display()
    );
    for name in ["gamescope", "wine"] {
        let path = bin.join(name);
        std::fs::write(&path, &script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fixture.extra_path = Some(bin.clone());
    fixture.start();

    // A game directory, an executable, and a wine prefix that looks real.
    let game_dir = fixture.dir.join("Game");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    let prefix = fixture.dir.join("prefix");
    std::fs::create_dir_all(prefix.join("drive_c/users/tester")).unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Launch Test", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "launch-test", "{response}");

    let response = fixture.rpc(
        "game.update",
        json!({
            "id": "launch-test",
            "launch_args": ["--windowed"],
            "wine_prefix": prefix,
        }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    // The fake gamescope exits instantly, so the daemon reports the immediate
    // exit — that error carries the command line it built.
    let response = fixture.rpc("game.launch", json!({ "id": "launch-test" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("gamescope")),
        "expected an immediate-exit error, got {response}"
    );

    // Wait for the fake to have written its probe.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !probe.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let probed = std::fs::read_to_string(&probe).expect("fake gamescope never ran");

    let field = |key: &str| {
        probed
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} missing from probe:\n{probed}"))
            .to_string()
    };

    // The game root is the working directory, and the prefix is exported.
    assert_eq!(field("cwd:"), game_dir.to_string_lossy());
    assert_eq!(field("WINEPREFIX:"), prefix.to_string_lossy());

    // gamescope gets the scaled profile, then `--`, then wine + exe + args.
    let argv = field("argv:");
    for expected in [
        "-w 1280 -h 720 -W 2560 -H 1440",
        "-S fit -F fsr --sharpness 12",
        "-- ",
    ] {
        assert!(argv.contains(expected), "missing {expected:?} in {argv:?}");
    }
    let (_, game_cmd) = argv.split_once(" -- ").expect("separator");
    let parts: Vec<&str> = game_cmd.split_whitespace().collect();
    assert!(parts[0].ends_with("bin/wine"), "wine first: {parts:?}");
    assert_eq!(parts[1], exe.to_string_lossy());
    assert_eq!(parts[2], "--windowed", "launch args are passed through");
}

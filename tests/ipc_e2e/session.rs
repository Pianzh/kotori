//! 启动游戏、跟随会话：gamescope 命令行与"只看着进程"那条路。

use std::time::{Duration, Instant};

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::{wait_until, write_script};

/// Is a process named `name` in the table right now?
///
/// Mirrors `process::matches` on the one rule that matters here: the exe's own
/// file name, either as `comm` or as the basename of `argv[0]`.
fn a_process_is_named(name: &str) -> bool {
    let base = |s: &str| {
        s.rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase()
    };
    let name = name.to_lowercase();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let comm = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
        let cmdline = std::fs::read_to_string(path.join("cmdline")).unwrap_or_default();
        let argv0 = cmdline.split('\0').next().unwrap_or_default();
        base(&comm) == name || (!argv0.is_empty() && base(argv0) == name)
    })
}

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
    // 新游戏的窗口尺寸**留空**(＝启动时按屏幕算,见 `ScaleProfile::output_size_for`):
    // 档案里不再记录某台机器的分辨率,换显示器/换机器都不用重扫。`null` 就是"自动"。
    assert!(created["scale_profile"]["output_width"].is_null());
    assert!(created["scale_profile"]["output_height"].is_null());

    // A duplicate name is no longer refused (2026-09-19: two entries for one
    // game are legal — "警告但不阻止"): the second one gets a suffixed id and
    // the response carries a warning about the shared exe.
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "My Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "my-game-2", "{response}");
    let warning = response["result"]["warning"].as_str().unwrap().to_string();
    assert!(warning.contains("My Game"), "{warning}");
    assert!(warning.contains("允许"), "{warning}");
    // A missing exe and a bad game dir are still refused.
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

    // Launching a watch-only game starts *following* its process instead.
    let response = fixture.rpc("game.launch", json!({ "id": "my-game" }));
    assert_eq!(response["result"]["watch_only"], true, "{response}");
    assert_eq!(response["result"]["process_name"], "MyGame.exe");
    let session = response["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    let status = fixture.rpc("daemon.status", json!({}));
    let sessions = status["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{status}");
    assert_eq!(sessions[0]["game_id"], "my-game");
    assert_eq!(sessions[0]["watch_only"], true);
    assert_eq!(sessions[0]["process_name"], "MyGame.exe");
    assert!(sessions[0]["gamescope_pid"].is_null());

    // Stopping a watch-only session drops it (nothing to kill).
    let response = fixture.rpc("game.stop", json!({ "session_id": session }));
    assert_eq!(response["result"]["success"], true, "{response}");
    assert!(
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A watch-only game without a process name cannot be tracked.
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Watchless", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "watchless", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "watchless", "watch_only": true }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");
    let response = fixture.rpc("game.launch", json!({ "id": "watchless" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("没有填写要观测的进程名"),
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
    let mut fixture = Fixture::new("launch");
    let bin = fixture.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let probe = fixture.dir.join("probe.txt");

    let script = format!(
        "#!/bin/sh\n[ \"$1\" = \"--kotori-warmup\" ] && exit 0\n{{ echo \"argv:$*\"; echo \"cwd:$(pwd)\"; echo \"WINEPREFIX:${{WINEPREFIX:-}}\"; }} >> '{}'\nexit 0\n",
        probe.display()
    );
    for name in ["gamescope", "wine"] {
        write_script(&bin.join(name), &script);
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
    // 新建的档案没写游戏分辨率 ⇒ 不发 `-w/-h`(gamescope 自己的默认值是 1280x720,
    // 用户 2026-09-13 决定不再让这一项预填)。
    let argv = field("argv:");
    for expected in ["-W 2560 -H 1440", "-S fit -F fsr --sharpness 12", "-- "] {
        assert!(argv.contains(expected), "missing {expected:?} in {argv:?}");
    }
    assert!(
        !argv.contains(" -w "),
        "游戏分辨率留空时不该发 -w/-h: {argv:?}"
    );
    let (_, game_cmd) = argv.split_once(" -- ").expect("separator");
    let parts: Vec<&str> = game_cmd.split_whitespace().collect();
    assert!(parts[0].ends_with("bin/wine"), "wine first: {parts:?}");
    assert_eq!(parts[1], exe.to_string_lossy());
    assert_eq!(parts[2], "--windowed", "launch args are passed through");
}

/// Watch-only sessions follow a real process: they must survive while it runs
/// and disappear once it exits.
#[test]
fn watch_only_session_follows_the_process() {
    let mut fixture = Fixture::new("watch");
    fixture.start();

    // A uniquely named copy of `sleep`, so the process name cannot collide with
    // anything else on the machine.
    let watched = fixture.dir.join("kotori-watched-proc");
    std::fs::copy("/bin/sleep", &watched).expect("copy /bin/sleep");
    let watched_name = watched.file_name().unwrap().to_string_lossy().to_string();

    let game_dir = fixture.dir.join("WatchGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Watch Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "watch-game", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "watch-game", "watch_only": true, "process_name": watched_name }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    // Nothing is running yet, so the session is created but still waiting.
    let response = fixture.rpc("game.launch", json!({ "id": "watch-game" }));
    assert_eq!(response["result"]["watch_only"], true, "{response}");

    let session_count = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(session_count(&fixture), 1);

    // Start the game ourselves — kotori never launches it.
    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");

    // Give the watcher a couple of poll intervals; the session must still be
    // there while the game runs.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        session_count(&fixture),
        1,
        "watching must survive a live game"
    );

    // Quitting the game ends the session.
    child.kill().unwrap();
    child.wait().unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    while session_count(&fixture) > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        session_count(&fixture),
        0,
        "session should end when the watched process exits"
    );
}

/// A direct launch (no gamescope) must end its session when the game exits.
///
/// The fake `wine` reproduces what real wine does to the process table: it
/// `exec -a`s itself so `argv[0]` *is* the game's exe path, which is the name
/// `process::matches` looks for. The session therefore has to end when that
/// process goes away — and `Ended` is what makes the save upload fire.
#[test]
fn direct_launch_session_ends_when_the_game_exits() {
    let mut fixture = Fixture::new("direct");
    let bin = fixture.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    let game_dir = fixture.dir.join("DirectGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();

    // Runs for 5s (well past the 300ms immediate-exit check), then exits.
    let script =
        "#!/bin/bash\n[ \"$1\" = \"--kotori-warmup\" ] && exit 0\nexec -a \"$1\" /bin/sleep 5\n";
    write_script(&bin.join("wine"), script);
    fixture.extra_path = Some(bin.clone());
    fixture.start();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Direct Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "direct-game", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "direct-game", "direct_launch": true }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    let session_count = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .len()
    };

    let response = fixture.rpc("game.launch", json!({ "id": "direct-game" }));
    assert!(
        response["result"]["session_id"].is_string(),
        "launch refused: {response}"
    );
    assert_eq!(session_count(&fixture), 1);

    // The fixture is faithful: the process table really does hold a `game.exe`
    // while the game runs, exactly like wine's rewritten `argv[0]`.
    assert!(
        wait_until(Duration::from_secs(10), || a_process_is_named("game.exe")),
        "the fake wine never showed up as game.exe — the fixture is not faithful"
    );

    // It exits on its own after 5s; the session must follow it out — not sit
    // there until the 300s "never appeared" timeout, which skips `Ended` and
    // with it the exit-time save upload.
    let ended = wait_until(Duration::from_secs(60), || session_count(&fixture) == 0);
    if !ended {
        let log = std::fs::read_to_string(&fixture.log).unwrap_or_default();
        panic!(
            "the session outlived the game it launched (still {} session(s))\n--- daemon log ---\n{log}",
            session_count(&fixture)
        );
    }
}

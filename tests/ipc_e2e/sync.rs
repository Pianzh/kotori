//! 云同步：上传、保留窗口、恢复，以及跟着游戏生命周期自动同步。
//!
//! 假 rclone 在临时目录里真搬文件，"桶"就是那棵目录树 —— 所以这些测试验的是
//! 真的打包、真的按清单合并，不是"调用参数长得对不对"。

use std::time::Duration;

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::{cloud_packages, rclone_calls, wait_until};

#[test]
fn cloud_sync_uploads_keeps_versions_and_restores_over_ipc() {
    let mut fixture = Fixture::new("sync");
    let remote = fixture.enable_fake_sync(true);
    fixture.start();

    // A game whose saves live in `<game_dir>/savedata`.
    let game_dir = fixture.dir.join("SyncGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("cg.dat"), b"first").unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Sync Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "sync-game", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "sync-game", "save_paths": ["savedata"] }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    // Nothing is set up yet, and the status says so instead of failing.
    let status = fixture.rpc("sync.status", json!({}));
    assert_eq!(status["result"]["ready"], false, "{status}");
    assert!(
        status["result"]["problem"]
            .as_str()
            .unwrap()
            .contains("B2 凭据"),
        "{status}"
    );
    assert_eq!(status["result"]["games"][1]["locations"], 1, "{status}");

    // --- credentials -------------------------------------------------------
    let response = fixture.rpc(
        "sync.set_credentials",
        json!({ "key_id": "test-key-id", "app_key": "test-app-key" }),
    );
    assert_eq!(response["result"]["stored"], true, "{response}");

    let status = fixture.rpc("sync.status", json!({}));
    let status_body = status.to_string();
    assert_eq!(status["result"]["ready"], true, "{status}");
    assert_eq!(status["result"]["remote"], "kotori:test-bucket/kotori");
    assert_eq!(status["result"]["secrets"][0], "b2-key-id");
    // The status may name the secrets; it must never carry their values.
    assert!(!status_body.contains("test-app-key"), "{status_body}");
    assert!(!status_body.contains("test-key-id"), "{status_body}");

    // The credentials reach rclone through the environment, never the argv.
    let response = fixture.rpc("sync.test", json!({}));
    assert_eq!(response["result"]["ok"], true, "{response}");
    for call in rclone_calls(&fixture) {
        assert!(!call.contains("test-app-key"), "{call}");
    }

    // --- first upload ------------------------------------------------------
    let response = fixture.rpc("sync.now", json!({ "id": "sync-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");
    assert_eq!(
        response["result"]["games"][0]["locations"][0]["action"], "uploaded",
        "{response}"
    );

    let packages = remote.join("games/sync-game");
    assert_eq!(
        cloud_packages(&packages).len(),
        1,
        "one version, one package"
    );

    // --- second upload: a second, complete package -------------------------
    std::fs::write(saves.join("cg.dat"), b"second").unwrap();
    let response = fixture.rpc("sync.now", json!({ "id": "sync-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");

    let versions = fixture.rpc("sync.versions", json!({ "id": "sync-game" }));
    let stamps: Vec<String> = versions["result"]["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stamp| stamp.as_str().unwrap().to_string())
        .collect();
    assert_eq!(stamps.len(), 2, "{versions}");
    assert_eq!(cloud_packages(&packages).len(), 2, "{versions}");
    // 名字就是时间序：第一个包是第一版。
    let first = stamps[0].clone();
    assert!(first.starts_with("20"), "{first}");

    // Sync also shows up in the status, so the UI can say when it last ran.
    let status = fixture.rpc("sync.status", json!({}));
    assert_eq!(status["result"]["games"][1]["last"]["ok"], true, "{status}");
    assert_eq!(status["result"]["games"][1]["last"]["action"], "上传");

    // --- restore the newest state -----------------------------------------
    std::fs::remove_dir_all(&saves).unwrap();
    let response = fixture.rpc("sync.restore", json!({ "id": "sync-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");
    assert_eq!(
        std::fs::read_to_string(saves.join("cg.dat")).unwrap(),
        "second",
        "a restore brings the newest package back"
    );

    // --- roll back to an older package -------------------------------------
    let response = fixture.rpc(
        "sync.restore",
        json!({ "id": "sync-game", "version": first }),
    );
    assert_eq!(response["result"]["ok"], true, "{response}");
    assert_eq!(
        std::fs::read_to_string(saves.join("cg.dat")).unwrap(),
        "first",
        "each package is a complete point in time, so rolling back just lays it \
         down\n{:?}",
        rclone_calls(&fixture)
    );

    // A version name that is not ours is refused before anything runs.
    let response = fixture.rpc(
        "sync.restore",
        json!({ "id": "sync-game", "version": "../../etc" }),
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("不是合法的版本名"),
        "{response}"
    );

    // --- retention stays off unless asked for ------------------------------
    let listing = rclone_calls(&fixture);
    assert!(
        !listing.iter().any(|call| call.starts_with("deletefile")),
        "keep_versions = 0 must never delete a version: {listing:?}"
    );
}

/// Sync rides on the session lifecycle: the saves are fetched before a launch
/// and pushed back after the game exits.
#[test]
fn save_sync_follows_the_game_lifecycle() {
    let mut fixture = Fixture::new("sync-life");
    let remote = fixture.enable_fake_sync(true);
    let probe = fixture.enable_fake_display();
    fixture.start();

    let game_dir = fixture.dir.join("LifeGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("save.dat"), b"from-cloud").unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Life Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "life-game", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "life-game", "save_paths": ["savedata"] }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");
    let response = fixture.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    assert_eq!(response["result"]["stored"], true, "{response}");

    let packages = remote.join("games/life-game");

    // Put something in the cloud, then remove the local copy.
    assert_eq!(
        fixture.rpc("sync.now", json!({ "id": "life-game" }))["result"]["ok"],
        true
    );
    std::fs::remove_dir_all(&saves).unwrap();
    assert!(!saves.exists());

    // Launching pulls it back *before* the game could read it. The fake
    // gamescope exits at once, so the launch itself fails — the point is that
    // the save is already back.
    let response = fixture.rpc("game.launch", json!({ "id": "life-game" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("gamescope")),
        "expected the fake gamescope to exit immediately: {response}"
    );
    assert!(
        probe.exists(),
        "the launch still reached gamescope despite sync"
    );
    assert_eq!(
        std::fs::read_to_string(saves.join("save.dat")).unwrap(),
        "from-cloud",
        "the pre-launch pull must run before the game starts"
    );

    // --- now the exit path, with a game kotori only watches -----------------
    // A uniquely named copy of `sleep`, so nothing else on the machine can be
    // mistaken for the game.
    let watched = fixture.dir.join("kotori-lifecycle-proc");
    std::fs::copy("/bin/sleep", &watched).unwrap();
    let watched_name = watched.file_name().unwrap().to_string_lossy().to_string();
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "life-game", "watch_only": true, "process_name": watched_name }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    let session = fixture.rpc("game.launch", json!({ "id": "life-game" }));
    assert_eq!(session["result"]["watch_only"], true, "{session}");

    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");
    // Let the engine notice it, then play "for a while".
    std::thread::sleep(Duration::from_secs(3));
    std::fs::write(saves.join("save.dat"), b"progress-made").unwrap();

    child.kill().unwrap();
    child.wait().unwrap();

    // The session ends, and the exit hook uploads what the game wrote as a new
    // package — the previous one is still there.
    assert!(
        wait_until(Duration::from_secs(30), || cloud_packages(&packages).len()
            >= 2),
        "the saves were never uploaded after the game exited ({} calls: {:?})",
        rclone_calls(&fixture).len(),
        rclone_calls(&fixture)
    );

    let versions = fixture.rpc("sync.versions", json!({ "id": "life-game" }));
    assert_eq!(
        versions["result"]["versions"].as_array().unwrap().len(),
        2,
        "{versions}"
    );

    // 退出后那一版就是最新的包：删掉本机存档再恢复，应该拿到游戏写下的进度。
    std::fs::remove_dir_all(&saves).unwrap();
    let response = fixture.rpc("sync.restore", json!({ "id": "life-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");
    assert_eq!(
        std::fs::read_to_string(saves.join("save.dat")).unwrap(),
        "progress-made",
        "the version uploaded after the exit is the newest one"
    );
}

/// 引擎是**单独一个字段**提交的：界面上点一下「kopia」发的就是 `{"engine": …}`，
/// daemon 按字段合并 —— 用户那些还没保存的 bucket/prefix 编辑一个都不该被碰掉。
///
/// 这条盯着 UI 那个"点一下就生效"改动的**前提**：`sync.set_settings` 哪天变成整份
/// 替换，界面上点一次引擎就会静默清掉用户正在填的 bucket。
///
/// 从前的问题更基本：点引擎**根本不提交**（只改表单，等「保存设置」），而按钮上已经
/// 显示成「kopia ✓」了 —— 用户 2026-09-16 报的"每次开 GUI 都回到 rclone"就是这么来的。
#[test]
fn setting_only_the_engine_leaves_the_other_settings_alone() {
    let mut fixture = Fixture::new("engine");
    fixture.enable_fake_sync(false);
    fixture.start();

    let seed = fixture.rpc(
        "sync.set_settings",
        json!({
            "enabled": false,
            "engine": "rclone",
            "bucket": "my-bucket",
            "prefix": "kotori",
            "keep_versions": 7
        }),
    );
    assert_eq!(seed["result"]["engine_changed"], false, "{seed}");

    // 界面上点「kopia」发的就是这个：一个键。
    let switched = fixture.rpc("sync.set_settings", json!({ "engine": "kopia" }));
    assert_eq!(switched["result"]["engine_changed"], true, "{switched}");
    let settings = &switched["result"]["settings"];
    assert_eq!(settings["engine"], "kopia", "{switched}");
    assert_eq!(settings["bucket"], "my-bucket", "这次提交不该碰别的字段");
    assert_eq!(settings["keep_versions"], 7, "{switched}");
    assert_eq!(settings["enabled"], false, "{switched}");

    // 而且真的落盘了 —— 重开 GUI 读到的就是它，不是 serde 的默认值 rclone。
    let status = fixture.rpc("sync.status", json!({}));
    assert_eq!(status["result"]["engine"], "kopia", "{status}");
    let written = std::fs::read_to_string(fixture.dir.join("config.toml")).unwrap();
    assert!(written.contains(r#"engine = "kopia""#), "{written}");
    assert!(written.contains(r#"bucket = "my-bucket""#), "{written}");
}

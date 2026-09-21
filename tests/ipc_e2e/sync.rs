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
    // ⚠ 真跑起来的必须**就是档案里那个 exe**:自动追踪认的是进程的 exe 完整路径
    // (用户 2026-09-20),所以这里把 `game.exe` 本身换成一个会一直跑下去的进程 ——
    // 上面那段用假 gamescope 的启动只把路径当参数,不看文件里是什么。
    // (新档案默认开着自动追踪,进程名默认就是 exe 的文件名,不用再设。)
    std::fs::copy("/bin/sleep", &exe).unwrap();

    // ⚠ 这里**不点「启动」**:自动追踪的意义就是"不是 kotori 启动的那一局也要跟"
    // (用户 2026-09-20),后台那圈轮询会自己认出它。
    let mut child = std::process::Command::new(&exe)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");
    let watched_session = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["game_id"] == "life-game")
    };
    assert!(
        wait_until(Duration::from_secs(20), || watched_session(&fixture)),
        "daemon 没有自己认出这个进程\n--- daemon log ---\n{}",
        fixture.logs()
    );

    // Then play "for a while".
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
/// 显示成「kopia √」了 —— 用户 2026-09-16 报的"每次开 GUI 都回到 rclone"就是这么来的。
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

    // **单独改桶名**也要落盘 —— 界面「连接与保留」那行的「保存设置」发的就是这一笔。
    // 用户 2026-09-21 报"打了字但没保存生效,重开又变回旧的":根因是他把框放在了
    // 「保存凭据」旁边(见 `sync.slint` 里那段注释),不是这一层;这条盯着 daemon 这半:
    // 收到桶名就该存进 config,重开 GUI 读到的才是新的。
    let renamed = fixture.rpc("sync.set_settings", json!({ "bucket": "renamed-bucket" }));
    assert_eq!(
        renamed["result"]["settings"]["bucket"], "renamed-bucket",
        "{renamed}"
    );
    let written = std::fs::read_to_string(fixture.dir.join("config.toml")).unwrap();
    assert!(
        written.contains(r#"bucket = "renamed-bucket""#),
        "桶名没落盘,重开当然还是旧的:\n{written}"
    );
}

/// 设置页里指的**程序位置**要真的被用上：填一个目录，daemon 就在里面找那个程序，
/// 并如实把它报进 `sync.status`。
///
/// 这一条是"不想配 PATH 的人"整件事的验收 —— 用户 2026-09-16 要的就是它：Windows 上
/// 把 kopia 解压到某个目录、PATH 里什么都不加，也该能用。
#[test]
fn a_configured_program_directory_is_where_the_daemon_looks() {
    let mut fixture = Fixture::new("bin-path");
    fixture.enable_fake_sync(false);
    fixture.start();

    // "装在别处"的 kopia：一个目录，里面是程序本体。
    let tools = fixture.dir.join("my-tools");
    std::fs::create_dir_all(&tools).unwrap();
    let kopia = tools.join("kopia");
    std::fs::write(&kopia, b"#!/bin/sh\nexit 0\n").unwrap();
    let asked = tools.to_str().unwrap().to_string();

    let response = fixture.rpc(
        "sync.set_settings",
        json!({ "enabled": false, "kopia_binary": asked }),
    );
    assert_eq!(
        response["result"]["settings"]["kopia_binary"], asked,
        "{response}"
    );

    // daemon 报的"当前生效的 kopia"就是目录里那一个 —— 不是 PATH 里的，也不是没有。
    let status = fixture.rpc("sync.status", json!({}));
    assert_eq!(
        status["result"]["kopia"],
        kopia.to_str().unwrap(),
        "{status}"
    );

    // 指了一个空目录则是"你填的位置不对"，而不是含糊的"PATH 里找不到"。
    // ⚠ 先存一对假凭据：没有它 `validate_secrets` 会抢在"程序位置"前面报"缺凭据"，
    // 而这一条测的恰恰是后者（problem 是一串 or_else，按顺序问）。
    let response = fixture.rpc(
        "sync.set_credentials",
        json!({ "key_id": "test-key-id", "app_key": "test-app-key" }),
    );
    assert_eq!(response["result"]["stored"], true, "{response}");

    let empty = fixture.dir.join("empty-tools");
    std::fs::create_dir_all(&empty).unwrap();
    fixture.rpc(
        "sync.set_settings",
        json!({ "enabled": true, "kopia_binary": empty.to_str().unwrap(), "engine": "kopia" }),
    );
    let status = fixture.rpc("sync.status", json!({}));
    let problem = status["result"]["problem"].as_str().unwrap_or_default();
    assert!(problem.contains("找不到 kopia"), "{status}");
    assert!(
        problem.contains(empty.to_str().unwrap()),
        "要说清是哪个位置不对：{problem}"
    );
}

/// `[sync]` 里**每一个**键，都要能真的改掉。
///
/// 单测那边有一条"daemon 收得下每个键"，但它管不了"字段收了、应用那一步忘了写"——
/// 那种 bug 里 `SettingsPatch` 一切正常，只有 config 没动（用户改了等于没改）。
/// 所以这里一路走到**值真的变了**，而且样本值必须与当前值**不同**：值一样的话，
/// "没生效"和"生效了"在断言上分不出来。
///
/// 键的清单不写死在这里，而是从 `SyncConfig` 自己取 —— 将来加字段，这条自动覆盖。
#[test]
fn every_sync_setting_can_actually_be_changed() {
    let mut fixture = Fixture::new("patch-keys");
    fixture.enable_fake_sync(false);
    fixture.start();

    // 每个键一个"与当前值不同"的样本。与 `SyncConfig::default()` 和
    // `enable_fake_sync` 写下的值都不能撞上。
    let samples = [
        ("enabled", json!(true)),
        ("engine", json!("kopia")),
        ("endpoint", json!("https://api001.backblazeb2.com")),
        ("bucket", json!("other-bucket")),
        ("prefix", json!("other-prefix")),
        ("keep_versions", json!(7)),
        ("rclone_binary", json!("/opt/rclone")),
        ("kopia_binary", json!("/opt/kopia")),
    ];

    for (key, sample) in &samples {
        let mut body = serde_json::Map::new();
        body.insert(key.to_string(), sample.clone());
        let response = fixture.rpc("sync.set_settings", serde_json::Value::Object(body));
        assert_eq!(
            &response["result"]["settings"][key], sample,
            "`{key}` 发出去了却没生效：{response}"
        );
    }

    // 而且落了盘 —— daemon 是唯一写者，重开一个进程也该读到这些值。
    let written = std::fs::read_to_string(fixture.dir.join("config.toml")).unwrap();
    for (key, _) in &samples {
        assert!(written.contains(key), "`{key}` 没写进 config：{written}");
    }
}

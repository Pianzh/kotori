//! 游戏库的增删改与缩放配置：都由守护进程经 IPC 落盘。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::assert_is_error;

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

// `game.update` 这条收口按用户 2026-09-20 的决定**先搁置**：硬拒绝当时只上了
// `game.create` 那条路（见 `game::exe_owner` 的注释）。用例先写好搁在这儿，
// 等收口落地时摘掉 `#[ignore]` —— 别让它长期红着，那会把别的回归信号一起淹掉。
#[ignore = "game.update 的 exe 收口搁置中,产品实现后摘掉这条"]
#[test]
fn updating_a_game_to_another_games_exe_is_refused() {
    let mut fixture = Fixture::new("duplicate-update");
    fixture.start();

    let first_dir = fixture.dir.join("first");
    let second_dir = fixture.dir.join("second");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    let first_exe = first_dir.join("game.exe");
    let second_exe = second_dir.join("game.exe");
    std::fs::write(&first_exe, b"first executable").unwrap();
    std::fs::write(&second_exe, b"second executable").unwrap();

    let first = fixture.rpc(
        "game.create",
        json!({ "name": "First", "exe_path": first_exe, "game_dir": first_dir }),
    );
    let first_id = first["result"]["id"].as_str().unwrap().to_string();
    let second = fixture.rpc(
        "game.create",
        json!({ "name": "Second", "exe_path": second_exe, "game_dir": second_dir }),
    );
    let second_id = second["result"]["id"].as_str().unwrap().to_string();

    let response = fixture.rpc(
        "game.update",
        json!({ "id": second_id, "exe_path": first_exe }),
    );
    assert!(
        response.get("error").is_some(),
        "game.update 也必须拒绝另一个档案已经拥有的 exe: {response}"
    );

    let games = fixture.rpc("game.list", json!({}))["result"]["games"].clone();
    let games = games.as_array().unwrap();
    let first = games
        .iter()
        .find(|game| game["id"].as_str() == Some(first_id.as_str()))
        .unwrap();
    let second = games
        .iter()
        .find(|game| game["id"].as_str() == Some(second_id.as_str()))
        .unwrap();
    assert_eq!(first["exe_path"], first_exe.to_string_lossy().as_ref());
    assert_eq!(second["exe_path"], second_exe.to_string_lossy().as_ref());
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

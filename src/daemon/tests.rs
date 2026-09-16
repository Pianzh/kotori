//! `daemon` 的单测：请求分发与 socket 认领。
//!
//! 从 `daemon/mod.rs` 拆出来（那边本来 700 多行）。

use super::*;

fn daemon() -> Daemon {
    Daemon::new(Config::default())
}

#[tokio::test]
async fn unknown_method_is_reported_as_method_not_found() {
    let reply = daemon()
        .handle_request(r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["error"]["code"], -32601);
    assert_eq!(value["id"], 7);
    assert!(!reply.shutdown);
}

#[tokio::test]
async fn malformed_json_is_a_parse_error() {
    let reply = daemon().handle_request("{not json").await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["error"]["code"], -32700);
}

#[tokio::test]
async fn wrong_jsonrpc_version_is_rejected() {
    let reply = daemon()
        .handle_request(r#"{"jsonrpc":"1.0","id":1,"method":"game.list"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["error"]["code"], -32600);
}

#[tokio::test]
async fn missing_params_are_invalid_params() {
    let reply = daemon()
        .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.launch"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["error"]["code"], -32602);
    assert!(value["error"]["message"].as_str().unwrap().contains("id"));
}

#[tokio::test]
async fn shutdown_reply_is_flagged() {
    let reply = daemon()
        .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.shutdown"}"#)
        .await;
    assert!(reply.shutdown);
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["success"], true);
}

#[tokio::test]
async fn game_list_exposes_the_full_scale_profile() {
    // Regression test: a hand-picked subset silently dropped sharpness,
    // fullscreen and framerate, so saving from the UI overwrote them.
    let mut config = Config::default();
    let profile = crate::config::ScaleProfile {
        algorithm: crate::config::ScaleAlgorithm::Nis { sharpness: 4 },
        framerate_limit: Some(60),
        force_fullscreen: false,
        // 显式填过的输出尺寸(留空＝自动,所以这里必须自己给),顺手一起验证
        // 它不会在 RPC 上被丢掉。
        output_width: Some(1920),
        output_height: Some(1080),
        ..crate::config::ScaleProfile::default_for()
    };
    config.games.insert(
        "demo".into(),
        crate::config::GameConfig {
            name: "demo".into(),
            game_dir: "/games/demo".into(),
            exe_path: "/games/demo/game.exe".into(),
            launch_args: Vec::new(),
            save_paths: Vec::new(),
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: profile,
            created_at: chrono::Utc::now(),
        },
    );

    let reply = Daemon::new(config)
        .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.list"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();

    let game = &value["result"]["games"][0];
    assert_eq!(game["id"], "demo");
    assert_eq!(game["scale_profile"]["algorithm"]["Nis"]["sharpness"], 4);
    assert_eq!(game["scale_profile"]["framerate_limit"], 60);
    assert_eq!(game["scale_profile"]["force_fullscreen"], false);
    assert_eq!(game["scale_profile"]["output_width"], 1920);
}

#[tokio::test]
async fn games_are_sorted_by_name() {
    let mut config = Config::default();
    for name in ["zeta", "alpha", "mid"] {
        config.games.insert(
            name.into(),
            crate::config::GameConfig {
                name: name.into(),
                game_dir: "/g".into(),
                exe_path: "/g/game.exe".into(),
                launch_args: Vec::new(),
                save_paths: Vec::new(),
                wine_prefix: None,
                watch_only: false,
                process_name: None,
                scale_profile: crate::config::ScaleProfile::default_for(),
                created_at: chrono::Utc::now(),
            },
        );
    }

    let reply = Daemon::new(config)
        .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"game.list"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    let names: Vec<&str> = value["result"]["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "mid", "zeta"]);
}

#[tokio::test]
async fn status_reports_no_sessions_initially() {
    let reply = daemon()
        .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.status"}"#)
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["running"], true);
    assert_eq!(value["result"]["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn status_on_unknown_session_lists_nothing_new() {
    // `scale.get_status` for a dead session must fail loudly instead of
    // reporting stale data from a cached copy.
    let reply = daemon()
            .handle_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"scale.get_status","params":{"session_id":"ghost"}}"#,
            )
            .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session not found")
    );
}

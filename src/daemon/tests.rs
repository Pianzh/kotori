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
            direct_launch: false,
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
                direct_launch: false,
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

/// 同名两条也要有**唯一确定**的顺序(用户 2026-09-18:「按 name 排序我认为应该是错的,
/// 你没有考虑 name 相同的极端状态」)。
///
/// 光按 `name` 排时,同名那两条谁在前由 `HashMap` 的遍历顺序决定 —— 它随插入历史与
/// 每次启动的随机种子变,于是"列表顺序"这件事就没有一个说法。id 是唯一的,拿它当
/// 第二关键字才得到全序;这条测试把同名的顺序钉在 id 的字母序上。
#[tokio::test]
async fn games_with_the_same_name_still_have_one_order() {
    let mut config = Config::default();
    for id in ["zzz", "aaa", "mmm"] {
        config.games.insert(
            id.into(),
            crate::config::GameConfig {
                name: "同名".into(),
                game_dir: "/g".into(),
                exe_path: "/g/game.exe".into(),
                launch_args: Vec::new(),
                save_paths: Vec::new(),
                wine_prefix: None,
                watch_only: false,
                direct_launch: false,
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
    let ids: Vec<&str> = value["result"]["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["aaa", "mmm", "zzz"]);
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

/// 同一条规矩用在游戏上：`GameConfig` 的每个键，`GamePatch` 要么收得下、要么在例外
/// 清单里**写明为什么**。
///
/// 和 `sync_rpc::tests` 里那条是一对（那边是 `[sync]`）。为什么要这样测：`GamePatch`
/// 也是按字段合并的，而 `deny_unknown_fields` 让"缺字段"从静默变成报错 —— 界面新加
/// 一项而这里没跟上时，这条会红。
///
/// ⚠ 它管不了"字段收了、应用那一步忘了写"那种：那得看值有没有真的变（游戏那边由
/// `tests/ipc_e2e` 的 `library_entries_are_managed_over_ipc` 覆盖，`[sync]` 那边是
/// `every_sync_setting_can_actually_be_changed`）。
///
/// 例外只有两个，都是故意的：
///   * `created_at` —— daemon 建游戏时自己写的时间戳，没有"让客户端改创建时间"这回事；
///   * `scale_profile` —— patch 里叫 `profile`（那个键只改缩放档案）。
#[test]
fn every_game_config_key_is_either_patchable_or_a_known_exception() {
    const EXCEPTIONS: [&str; 2] = ["created_at", "scale_profile"];
    /// config 与 patch 里名字不一样的那几个：`(config 里的, patch 里的)`。
    const RENAMED: [(&str, &str); 1] = [("scale_profile", "profile")];

    let config = serde_json::to_value(crate::config::GameConfig {
        name: "x".into(),
        game_dir: "/g".into(),
        exe_path: "/g/x.exe".into(),
        launch_args: Vec::new(),
        save_paths: Vec::new(),
        wine_prefix: None,
        watch_only: false,
        direct_launch: false,
        process_name: None,
        scale_profile: crate::config::ScaleProfile::default_for(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    for (key, sample) in config.as_object().expect("GameConfig 该是个表") {
        if EXCEPTIONS.contains(&key.as_str()) {
            continue;
        }
        let name = RENAMED
            .iter()
            .find(|(from, _)| from == key)
            .map_or(key.as_str(), |(_, to)| *to);
        let mut body = serde_json::Map::new();
        body.insert(name.to_string(), sample.clone());
        serde_json::from_value::<protocol::GamePatch>(Value::Object(body)).unwrap_or_else(|e| {
            panic!("daemon 收不下 GameConfig 的 `{key}`（patch 里叫 `{name}`）：{e}")
        });
    }
}

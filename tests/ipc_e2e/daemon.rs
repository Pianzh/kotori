//! 守护进程自己的生命周期：起得来、抢不走别人的 socket、坏 socket 能自己收拾。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::assert_is_error;

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

/// 一个**还活着**的守护进程不能被第二个顶掉。
///
/// 这不是洁癖:两个守护进程会同时写同一份 `config.toml`,而"daemon 是唯一配置写者"
/// 是这套架构的基石(ADR-002)。这条路以前会在 bind 前无条件删掉 socket 文件,于是
/// 第二个把第一个的 socket 抢走 —— 第一个**还在跑**(游戏还在它手里、还会写配置),
/// 但界面与 CLI 再也找不到它。现在由 `<socket>.lock` 上的 `flock` 挡住。
#[test]
fn a_second_daemon_refuses_to_steal_a_live_socket() {
    let mut fixture = Fixture::new("socket-owned");
    fixture.start();
    assert_eq!(
        fixture.rpc("daemon.status", json!({}))["result"]["running"],
        true
    );

    // 第二次启动:同一份配置、同一个 socket。
    let output = Command::new(env!("CARGO_BIN_EXE_kotori"))
        .arg("daemon")
        .env("KOTORI_CONFIG", &fixture.config)
        .env("KOTORI_SOCKET", &fixture.socket)
        .stdin(Stdio::null())
        .output()
        .expect("第二个守护进程应当启动后拒绝");

    assert!(
        !output.status.success(),
        "第二个守护进程必须失败退出,而不是抢走 socket"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("已经有一个守护进程"),
        "拒绝的理由要说清楚(现在这样没法排查):{stderr}"
    );

    // 第一个必须毫发无损、还连着。
    assert_eq!(
        fixture.rpc("daemon.status", json!({}))["result"]["running"],
        true,
        "第一个守护进程被顶掉了"
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

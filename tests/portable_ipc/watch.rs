//! 停止自动追踪只影响当前进程实例；重新启动同一 exe 后必须恢复追踪。

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use super::fixture::{ChildGuard, Fixture, wait_until};

#[test]
fn stop_observing_keeps_game_alive_and_next_process_is_observed() {
    let mut fixture = Fixture::new("watch-stop");
    fixture.start();
    let exe = fixture.game_exe();
    let response = fixture.rpc("game.create", json!({"name":"Observed", "exe_path":exe}));
    assert_eq!(response["result"]["id"], "observed", "{response}");
    let ready = fixture.dir.join("ready");
    let release = fixture.dir.join("release");
    let spawn = || {
        ChildGuard(
            Command::new(&exe)
                .arg("--game")
                .arg(&ready)
                .arg(&release)
                .spawn()
                .unwrap(),
        )
    };
    let session = || {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["game_id"] == "observed")
            .and_then(|s| s["session_id"].as_str())
            .map(str::to_owned)
    };
    let mut first = spawn();
    assert!(
        wait_until(Duration::from_secs(30), || session().is_some()),
        "first game not observed"
    );
    let stopped = fixture.rpc("game.stop", json!({"session_id":session().unwrap()}));
    assert_eq!(stopped["result"]["success"], true, "{stopped}");
    // 跨过至少三个自动追踪周期，防止只测到 stop 后的瞬间空档。
    let until = Instant::now() + Duration::from_secs(7);
    while Instant::now() < until {
        assert!(
            first.0.try_wait().unwrap().is_none(),
            "stopping observation killed game"
        );
        assert!(session().is_none(), "same process was observed again");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::write(&release, b"exit").unwrap();
    assert!(wait_until(Duration::from_secs(10), || first
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(first.0.wait().unwrap().success());
    std::fs::remove_file(&release).unwrap();
    std::fs::remove_file(&ready).unwrap();
    let mut next = spawn();
    assert_ne!(
        first.0.id(),
        next.0.id(),
        "fixture requires a new process instance"
    );
    assert!(
        wait_until(Duration::from_secs(30), || session().is_some()),
        "new process stayed ignored\n{}",
        fixture.logs()
    );
    std::fs::write(&release, b"exit").unwrap();
    assert!(wait_until(Duration::from_secs(10), || next
        .0
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(next.0.wait().unwrap().success());
    assert!(wait_until(Duration::from_secs(30), || session().is_none()));
    fixture.shutdown();
}

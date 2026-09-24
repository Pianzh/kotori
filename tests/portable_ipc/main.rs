//! 两端共用的真 daemon 契约，平台差异只在传输和假 wine 适配。

mod cli;
mod engines;
mod fixture;
mod lifecycle;
#[path = "../support/native.rs"]
mod native;
mod watch;

use std::time::Duration;

use fixture::{Fixture, wait_until};
use serde_json::json;

#[test]
fn configuration_survives_restart_and_second_daemon_cannot_steal_endpoint() {
    let mut fixture = Fixture::new("restart");
    fixture.start();
    let exe = fixture.game_exe();
    let created = fixture.rpc("game.create", json!({"name":"Portable", "exe_path":exe}));
    assert_eq!(created["result"]["id"], "portable", "{created}");
    let changed = fixture.rpc(
        "game.update",
        json!({"id":"portable", "name":"Saved Name", "auto_watch":false, "direct_launch":true}),
    );
    assert_eq!(changed["result"]["success"], true, "{changed}");

    let mut other = fixture.spawn_daemon("second.log");
    assert!(
        wait_until(Duration::from_secs(10), || other
            .0
            .try_wait()
            .unwrap()
            .is_some()),
        "second daemon kept running"
    );
    assert!(
        !other.0.wait().unwrap().success(),
        "second daemon stole endpoint"
    );
    assert_eq!(
        fixture.rpc("daemon.status", json!({}))["result"]["games"],
        1
    );
    fixture.shutdown();
    fixture.start();
    let response = fixture.rpc("game.list", json!({}));
    let games = response["result"]["games"].as_array().unwrap();
    assert_eq!(games.len(), 1, "{response}");
    assert_eq!(games[0]["name"], "Saved Name");
    assert_eq!(games[0]["direct_launch"], true);
    assert_eq!(games[0]["auto_watch"], false);
    fixture.shutdown();
}

#[test]
fn concurrent_clients_receive_their_own_replies() {
    let mut fixture = Fixture::new("clients");
    fixture.start();
    std::thread::scope(|scope| {
        for _ in 0..12 {
            let fixture = &fixture;
            scope.spawn(move || {
                for _ in 0..8 {
                    let response = fixture.rpc("daemon.status", json!({}));
                    assert_eq!(response["result"]["running"], true, "{response}");
                    assert_eq!(response["result"]["games"], 0, "{response}");
                }
            });
        }
    });
    fixture.shutdown();
}

//! 从真实进程生命周期一直验证到云端字节；ready/release 握手避免固定 sleep 窗口。

use std::io::Read;
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use super::fixture::{ChildGuard, Fixture, wait_until};

fn has_session(fixture: &Fixture) -> bool {
    fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["game_id"] == "journey")
}

fn journey(observe: bool) {
    let mut fixture = Fixture::new(if observe { "observe" } else { "direct" });
    fixture.start();
    let exe = fixture.game_exe();
    let ready = fixture.dir.join("ready");
    let release = fixture.dir.join("release");
    let saves = fixture.dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let response = fixture.rpc("game.create", json!({"name":"Journey", "exe_path":exe}));
    assert_eq!(response["result"]["id"], "journey", "{response}");
    let args = json!(["--game", ready, release]);
    let response = fixture.rpc("game.update", json!({"id":"journey", "direct_launch":true, "auto_watch":observe, "launch_args":args, "save_paths":["savedata"]}));
    assert_eq!(response["result"]["success"], true, "{response}");
    let response = fixture.rpc(
        "sync.set_credentials",
        json!({"key_id":"fixture-id", "app_key":"fixture-key"}),
    );
    assert_eq!(response["result"]["stored"], true, "{response}");

    let mut game = if observe {
        Some(ChildGuard(
            Command::new(&exe)
                .arg("--game")
                .arg(&ready)
                .arg(&release)
                .spawn()
                .unwrap(),
        ))
    } else {
        let response = fixture.rpc("game.launch", json!({"id":"journey"}));
        assert!(response["result"]["session_id"].is_string(), "{response}");
        None
    };
    assert!(
        wait_until(Duration::from_secs(15), || ready.is_file()),
        "game did not signal ready"
    );
    assert!(
        wait_until(Duration::from_secs(30), || has_session(&fixture)),
        "session never appeared\n{}",
        fixture.logs()
    );
    // 只有确认进程和会话都活着，才模拟本次游戏写入并释放进程。
    let payload = b"new progress\0\xff\n";
    std::fs::write(saves.join("progress.dat"), payload).unwrap();
    std::fs::write(&release, b"exit").unwrap();
    if let Some(child) = game.as_mut() {
        assert!(wait_until(Duration::from_secs(10), || child
            .0
            .try_wait()
            .unwrap()
            .is_some()));
        assert!(child.0.wait().unwrap().success());
    }
    assert!(
        wait_until(Duration::from_secs(30), || !has_session(&fixture)),
        "session outlived game"
    );
    let packages = fixture.dir.join("bucket/fixture/saves/games/journey");
    // ⚠ 先等**桶里出现包**：这是纯文件系统观察，一个 RPC 都不发。从前这里是在
    // `wait_until` 里每 50ms 问一次 `sync.status`，几百个请求堆在一起 —— Windows
    // 上每个 `sync.status` 背后都要探一遍引擎，几十上百发叠起来，总有一发会撞上
    // fixture 的超时（2026-09-25 连着几轮挂在 `sync.status` 上，而同一份代码在
    // Linux 上全绿）。同步点该是**直接证据**，不是把 daemon 压垮。
    assert!(
        wait_until(Duration::from_secs(30), || {
            std::fs::read_dir(&packages)
                .map(|entries| {
                    entries
                        .flatten()
                        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "zip"))
                })
                .unwrap_or(false)
        }),
        "exit upload did not publish a package\n{}",
        fixture.logs()
    );
    // 包已经在那儿了，再问一次 daemon 的记账（一次请求，不是几百次）。
    let response = fixture.rpc("sync.status", json!({}));
    assert!(
        response["result"]["games"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| {
                g["id"] == "journey" && g["last"]["ok"] == true && g["last"]["action"] == "上传"
            }),
        "sync.status 没记下这次上传: {response}\n{}",
        fixture.logs()
    );
    let files: Vec<_> = std::fs::read_dir(&packages)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "zip"))
        .collect();
    assert_eq!(files.len(), 1, "one exit should publish one package");
    // 独立读取 ZIP，防止上传与恢复两端共享错误却互相抵消。
    let mut archive = zip::ZipArchive::new(std::fs::File::open(files[0].path()).unwrap()).unwrap();
    let mut found = 0;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        if file.name().ends_with("/progress.dat") {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, payload);
            found += 1;
        }
    }
    assert_eq!(found, 1, "uploaded archive lacks game progress");
    fixture.shutdown();
}

#[test]
fn direct_exit_uploads_the_actual_save_bytes() {
    journey(false);
}

#[test]
fn externally_started_game_uploads_after_exit() {
    journey(true);
}

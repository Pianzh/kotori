//! 自动追踪的端到端:「不是 kotori 启动的那一局也要跟」。
//!
//! 从 `session.rs` 拆出来(那边连着这两个测试会越过 500 行的软线)。测的是同一件事
//! 的两面:**谁在跑要自己认出来**(用户 2026-09-20),以及**一个进程只归一款游戏**。

use std::time::{Duration, Instant};

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::wait_until;

#[test]
fn auto_watch_follows_a_game_kotori_did_not_launch() {
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
        json!({ "id": "watch-game", "auto_watch": true, "process_name": watched_name }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    // ⚠ 这里**故意不点「启动」**。自动追踪的意思就是"不是 kotori 启动的那一局
    // 也要跟"(用户 2026-09-20) —— 从前必须手点一次「启动」(那一按什么都不启动,
    // 只是让 daemon 开始盯),于是双击图标玩的那一局在库里什么都不留。
    let session_count = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(session_count(&fixture), 0, "还没开游戏,不该有会话");

    // Start the game ourselves — kotori never launches it.
    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");

    // 后台那圈轮询要自己发现它(两个周期 + 余量)。
    assert!(
        wait_until(Duration::from_secs(20), || session_count(&fixture) == 1),
        "daemon 没有自己认出这个进程\n--- daemon log ---\n{}",
        fixture.logs()
    );

    // 在游戏跑着的时候,会话必须一直在。
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        session_count(&fixture),
        1,
        "watching must survive a live game"
    );

    // 用户点「停止」= 这一局别再跟了。**它不能被后台那圈轮询自己撤销** ——
    // 会话是循环认出来的,不记一笔的话两秒后就会长回来。
    let session_id = fixture.rpc("daemon.status", json!({}))["result"]["sessions"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = fixture.rpc("game.stop", json!({ "session_id": session_id }));
    assert_eq!(response["result"]["success"], true, "{response}");
    assert_eq!(session_count(&fixture), 0, "{response}");
    std::thread::sleep(Duration::from_secs(2 * 3));
    assert_eq!(
        session_count(&fixture),
        0,
        "点过停止的观测会话不许自己长回来"
    );

    // 进程走光之后解禁 —— 下一局照样自动跟(停止是"这一局",不是永久的)。
    child.kill().unwrap();
    child.wait().unwrap();
    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process again");
    assert!(
        wait_until(Duration::from_secs(20), || session_count(&fixture) == 1),
        "下一局没有被重新认出来\n--- daemon log ---\n{}",
        fixture.logs()
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

#[test]
fn one_process_is_claimed_by_a_single_game() {
    let mut fixture = Fixture::new("claim");
    fixture.start();

    let watched = fixture.dir.join("kotori-claim-proc");
    std::fs::copy("/bin/sleep", &watched).expect("copy /bin/sleep");
    let watched_name = watched.file_name().unwrap().to_string_lossy().to_string();

    let game_dir = fixture.dir.join("ClaimGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();

    // 两款不同的游戏,盯同一个进程名(现实里是"exe 同名",这里是同一个名字)。
    for (name, id) in [("First", "first"), ("Second", "second")] {
        let response = fixture.rpc(
            "game.create",
            json!({ "name": name, "exe_path": exe, "game_dir": game_dir }),
        );
        assert_eq!(response["result"]["id"], id, "{response}");
        let response = fixture.rpc(
            "game.update",
            json!({ "id": id, "auto_watch": true, "process_name": watched_name }),
        );
        assert_eq!(response["result"]["success"], true, "{response}");
    }

    let session_count = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .len()
    };

    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");
    assert!(
        wait_until(Duration::from_secs(20), || session_count(&fixture) == 1),
        "daemon 没有自己认出这个进程\n--- daemon log ---\n{}",
        fixture.logs()
    );

    // 再多等几轮:第二个会话不许后来才冒出来。
    std::thread::sleep(Duration::from_secs(2 * 4));
    assert_eq!(
        session_count(&fixture),
        1,
        "同一个进程只该有一个会话\n--- daemon log ---\n{}",
        fixture.logs()
    );
    assert!(
        fixture.logs().contains("同时匹配多款"),
        "歧义要在日志里说清楚:\n{}",
        fixture.logs()
    );

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(15), || session_count(&fixture) == 0),
        "进程退出后会话要收掉"
    );
}

/// 「跟这一局」:用户直接指一个 pid,精确到不会认错同名的另一款。
///
/// 名字给不了这种精确 —— 自动追踪那条路在同名时只能按 id 排序取第一个(见
/// `one_process_is_claimed_by_a_single_game`);pid 是"就是这一个进程"。
#[test]
fn observing_one_pid_picks_exactly_that_game() {
    let mut fixture = Fixture::new("observe");
    fixture.start();

    let watched = fixture.dir.join("kotori-observe-proc");
    std::fs::copy("/bin/sleep", &watched).expect("copy /bin/sleep");
    let watched_name = watched.file_name().unwrap().to_string_lossy().to_string();

    let game_dir = fixture.dir.join("ObserveGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();

    // 两款游戏(名字一样、进程名也一样),谁都**没有**开自动追踪:这一局只由 pid 决定。
    for (name, id) in [("First", "first"), ("Second", "second")] {
        let response = fixture.rpc(
            "game.create",
            json!({ "name": name, "exe_path": exe, "game_dir": game_dir }),
        );
        assert_eq!(response["result"]["id"], id, "{response}");
        let response = fixture.rpc(
            "game.update",
            json!({ "id": id, "auto_watch": false, "process_name": watched_name }),
        );
        assert_eq!(response["result"]["success"], true, "{response}");
    }

    let sessions = |fixture: &Fixture| -> Vec<String> {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["game_id"].as_str().unwrap_or("?").to_string())
            .collect()
    };

    // 还没开游戏:没什么可跟的。不存在的 pid 要如实报错,别开出一个空会话。
    let response = fixture.rpc("game.observe", json!({ "id": "first", "pid": 999_999 }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("已经不在了"),
        "{response}"
    );
    assert!(sessions(&fixture).is_empty());

    let mut child = std::process::Command::new(&watched)
        .arg("30")
        .spawn()
        .expect("spawn the watched process");
    let pid = child.id() as i64;

    let response = fixture.rpc("game.observe", json!({ "id": "second", "pid": pid }));
    assert!(
        response["result"]["session_id"].is_string(),
        "observe refused: {response}"
    );
    assert_eq!(
        response["result"]["process_name"], watched_name,
        "{response}"
    );
    assert_eq!(sessions(&fixture), vec!["second".to_string()], "{response}");

    // 同一款再挑一次会被挡下(已经有一个会话在跟它了)。
    let response = fixture.rpc("game.observe", json!({ "id": "second", "pid": pid }));
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("已经在跟"),
        "{response}"
    );

    // 进程退出 → 会话结束(退出后上传就挂在这上面)。
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(15), || sessions(&fixture).is_empty()),
        "按 pid 跟的会话没有跟着进程结束\n--- daemon log ---\n{}",
        fixture.logs()
    );
}

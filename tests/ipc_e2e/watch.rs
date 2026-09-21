//! 自动追踪的端到端:「不是 kotori 启动的那一局也要跟」。
//!
//! 从 `session.rs` 拆出来(那边连着这几个测试会越过 500 行的软线)。测的是同一件事
//! 的两面:**谁在跑要自己认出来**(用户 2026-09-20),以及**同一个 exe 只归一款游戏**。
//!
//! ⚠ 认人的凭据是进程的 **exe 完整路径**(不是名字),所以这里的"游戏"都是**真跑起来
//! 的那个文件本身**,而不是一个空壳 exe 配一个另外起的进程。

use std::time::{Duration, Instant};

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::wait_until;

#[test]
fn auto_watch_follows_a_game_kotori_did_not_launch() {
    let mut fixture = Fixture::new("watch");
    fixture.start();

    // ⚠ **档案里的 exe 必须就是真跑起来的那个文件**:自动追踪认人的凭据是进程的
    // exe 完整路径(用户 2026-09-20:任务管理器里那一栏就是它)。从前这里记的是一个
    // 空的 `game.exe`、真跑的却是另一个名字 —— 那种"按名字认"的写法已经删掉了。
    let exe = fixture.dir.join("kotori-watched-proc");
    std::fs::copy("/bin/sleep", &exe).expect("copy /bin/sleep");

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Watch Game", "exe_path": exe }),
    );
    assert_eq!(response["result"]["id"], "watch-game", "{response}");
    // 什么都不用再设:新档案默认开着自动追踪,要认的进程名默认就是 exe 的文件名。

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
    let mut child = std::process::Command::new(&exe)
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
    let mut child = std::process::Command::new(&exe)
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

    // 真跑起来的就是档案里那个 exe(见上一条测试的说明)。
    let exe = fixture.dir.join("kotori-claim-proc");
    std::fs::copy("/bin/sleep", &exe).expect("copy /bin/sleep");
    let watched_name = exe.file_name().unwrap().to_string_lossy().to_string();

    let game_dir = fixture.dir.join("ClaimGame");
    std::fs::create_dir_all(&game_dir).unwrap();
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "First", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "first", "{response}");

    // ① 同一个 exe **建不出第二条档案**(用户 2026-09-20:两条档案指着同一个 exe 会让
    //    云端的版本历史分家,不如挡住)。错误里要说清撞的是谁。
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Second", "exe_path": exe, "game_dir": game_dir }),
    );
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("已经属于「First」"), "{response}");

    // ② 旧配置里"两条档案指着同一个 exe"毕竟还可能出现(改 exe 那条路按用户的话先
    //    搁置,没挡)。那时后台循环只能跟一条,而且必须在日志里说清 —— 否则一个进程
    //    会开出两个会话,退出时会传两次存档。
    //
    //    这里就用"先建一条别的、再把 exe 改过来"复现那种旧配置:`watched_name` 只是
    //    让它有个名字,判据始终是 exe 完整路径。
    let other_exe = fixture.dir.join("kotori-claim-other");
    std::fs::write(&other_exe, b"").unwrap();
    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Second", "exe_path": other_exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "second", "{response}");
    let response = fixture.rpc(
        "game.update",
        json!({ "id": "second", "exe_path": exe, "process_name": watched_name }),
    );
    assert_eq!(response["result"]["success"], true, "{response}");

    let session_count = |fixture: &Fixture| {
        fixture.rpc("daemon.status", json!({}))["result"]["sessions"]
            .as_array()
            .unwrap()
            .len()
    };

    let mut child = std::process::Command::new(&exe)
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
        fixture.logs().contains("已经被别的档案认领"),
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

/// `process.list`:添加游戏页「从运行中的进程添加」的候选列表。
///
/// 它必须**短**:只列看着像游戏的进程 —— 把上百个后台进程倒给用户等于什么也没说。
/// 而每一条都要带着"能直接拿来用"的东西:PID(跟这一局)与 exe 路径(建条目)。
#[test]
fn the_process_list_offers_usable_candidates_only() {
    let mut fixture = Fixture::new("pickable");
    fixture.start();

    let dir = fixture.dir.join("running");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("kotori-pickable-probe.exe");
    std::fs::copy("/bin/sleep", &exe).expect("copy /bin/sleep");
    let mut child = std::process::Command::new(&exe)
        .arg("30")
        .spawn()
        .expect("spawn the probe");
    let pid = child.id() as i64;

    let listed = |fixture: &Fixture| -> Vec<serde_json::Value> {
        fixture.rpc("process.list", json!({}))["result"]["processes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };

    assert!(
        wait_until(Duration::from_secs(10), || listed(&fixture)
            .iter()
            .any(|entry| entry["pid"] == pid)),
        "刚起的进程没出现在候选里\n--- daemon log ---\n{}",
        fixture.logs()
    );

    let entry = listed(&fixture)
        .into_iter()
        .find(|entry| entry["pid"] == pid)
        .unwrap();
    assert_eq!(entry["name"], "kotori-pickable-probe.exe");
    // exe 路径要能直接填进「添加游戏」(Linux 这边来自 argv0)。
    assert_eq!(entry["exe"], exe.to_string_lossy().as_ref());

    // wine 那层管道进程不该混进来 —— 挑了它毫无意义。
    for entry in listed(&fixture) {
        let name = entry["name"].as_str().unwrap_or_default();
        assert!(
            !matches!(name, "wineserver" | "wine" | "gamescope"),
            "管道进程混进候选了: {entry}"
        );
    }

    // 进程退掉之后就不该再列它(列表是"此刻"的快照)。
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || !listed(&fixture)
            .iter()
            .any(|entry| entry["pid"] == pid)),
        "退掉的进程还在候选里"
    );
}

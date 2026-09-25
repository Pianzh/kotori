//! 实际执行 CLI：输出、退出码和落盘结果必须相互一致。

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use super::fixture::{ChildGuard, Fixture, wait_until};

struct Reply {
    ok: bool,
    out: String,
    err: String,
}

fn run(fixture: &Fixture, args: &[&str]) -> Reply {
    // 文件承接输出，避免管道写满让 wait 与 read 相互等待。
    let id = uuid::Uuid::new_v4();
    let out = fixture.dir.join(format!("cli-{id}.stdout"));
    let err = fixture.dir.join(format!("cli-{id}.stderr"));
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_kotori"))
            .args(args)
            .env("KOTORI_CONFIG", fixture.dir.join("config.toml"))
            .env("KOTORI_SOCKET", fixture.endpoint())
            .env("KOTORI_DATA_DIR", fixture.dir.join("data"))
            .env(
                "KOTORI_SECRETS_FILE",
                fixture.dir.join("secrets/credentials.enc"),
            )
            .env("KOTORI_SECRET_TOOL", fixture.dir.join("no-secret-tool"))
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&out).unwrap())
            .stderr(std::fs::File::create(&err).unwrap())
            .spawn()
            .unwrap(),
    );
    assert!(
        wait_until(Duration::from_secs(30), || child
            .0
            .try_wait()
            .unwrap()
            .is_some()),
        "CLI timeout: {args:?}"
    );
    let reply = Reply {
        ok: child.0.wait().unwrap().success(),
        out: std::fs::read_to_string(out).unwrap(),
        err: std::fs::read_to_string(err).unwrap(),
    };
    eprintln!(
        "CLI {args:?}: ok={}\n{}\n{}",
        reply.ok, reply.out, reply.err
    );
    reply
}

#[test]
fn help_and_invalid_arguments_have_correct_exit_status() {
    let fixture = Fixture::new("cli-parse");
    let help = run(&fixture, &["--help"]);
    assert!(help.ok);
    assert!(help.out.contains("sync") && help.out.contains("launch"));
    for args in [
        vec!["unknown-command"],
        vec!["launch"],
        vec!["sync", "restore"],
        vec!["sync", "now", "--typo"],
    ] {
        let reply = run(&fixture, &args);
        assert!(!reply.ok, "accepted {args:?}");
        assert!(!reply.err.is_empty());
    }
}

#[test]
fn status_and_reload_report_missing_daemon_without_starting_one() {
    let fixture = Fixture::new("cli-offline");
    for action in ["status", "reload", "shutdown"] {
        let reply = run(&fixture, &[action]);
        assert!(!reply.ok, "{action} unexpectedly succeeded");
        assert!(reply.out.contains("守护进程未响应"));
    }
}

#[test]
fn sync_status_starts_a_missing_daemon() {
    let fixture = Fixture::new("cli-autostart");
    let status = run(&fixture, &["sync", "status"]);
    assert!(status.ok, "stdout={} stderr={}", status.out, status.err);

    let shutdown = fixture.rpc("daemon.shutdown", json!({}));
    assert!(shutdown.get("error").is_none(), "{shutdown}");
}

#[test]
fn negative_sharpness_is_a_valid_argument_and_reaches_session_validation() {
    let mut fixture = Fixture::new("cli-negative");
    fixture.start();
    let reply = run(&fixture, &["scale", "sharpness", "-1"]);
    assert!(!reply.ok, "there is no active session");
    assert!(
        !reply.err.contains("unexpected argument"),
        "documented negative argument rejected by parser: {}",
        reply.err
    );
    assert!(
        !reply.err.contains("Usage:"),
        "argument parsing failed: {}",
        reply.err
    );
    fixture.shutdown();
}

#[test]
fn scan_is_read_only_and_repeated_add_preserves_game_settings() {
    let mut fixture = Fixture::new("cli-library");
    fixture.start();
    let directory = fixture.dir.join("中文 library");
    let game = directory.join("Example Game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("Game.exe"), b"fixture executable").unwrap();
    let before = std::fs::read(fixture.dir.join("config.toml")).unwrap();
    let scan = run(&fixture, &["scan", directory.to_str().unwrap()]);
    assert!(scan.ok && scan.out.contains("Game.exe"));
    assert_eq!(
        std::fs::read(fixture.dir.join("config.toml")).unwrap(),
        before
    );
    // `add` 走 daemon（唯一写者），所以**不用** reload 就已经在库里了 —— 从前它
    // 直接改磁盘，daemon 内存里的库要等下一次 reload 才知道（BUG-16）。
    assert!(run(&fixture, &["add", directory.to_str().unwrap()]).ok);
    let response = fixture.rpc("game.list", json!({}));
    let games = response["result"]["games"].as_array().unwrap();
    assert_eq!(games.len(), 1, "{response}");
    let id = games[0]["id"].as_str().unwrap();
    assert_eq!(
        fixture.rpc(
            "game.update",
            json!({"id":id, "name":"User Label", "auto_watch":false, "direct_launch":true})
        )["result"]["success"],
        true
    );
    assert!(run(&fixture, &["add", directory.to_str().unwrap()]).ok);
    let response = fixture.rpc("game.list", json!({}));
    let games = response["result"]["games"].as_array().unwrap();
    assert_eq!(games.len(), 1, "duplicate created: {response}");
    assert_eq!(games[0]["name"], "User Label");
    assert_eq!(games[0]["direct_launch"], true);
    assert_eq!(games[0]["auto_watch"], false);
    let listed = run(&fixture, &["list"]);
    assert!(listed.ok && listed.out.contains("User Label"));
    fixture.shutdown();
}

#[test]
fn cli_upload_versions_and_restore_round_trip_actual_bytes() {
    let mut fixture = Fixture::new("cli-sync");
    fixture.start();
    let exe = fixture.game_exe();
    assert_eq!(
        fixture.rpc("game.create", json!({"name":"CLI Game", "exe_path":exe}))["result"]["id"],
        "cli-game"
    );
    assert_eq!(
        fixture.rpc(
            "game.update",
            json!({"id":"cli-game", "auto_watch":false, "save_paths":["savedata"]})
        )["result"]["success"],
        true
    );
    assert_eq!(
        fixture.rpc(
            "sync.set_credentials",
            json!({"key_id":"fixture-id", "app_key":"fixture-key"})
        )["result"]["stored"],
        true
    );
    let saves = fixture.dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let save = saves.join("中文 progress.dat");
    std::fs::write(&save, b"original\0\xff").unwrap();
    let status = run(&fixture, &["status"]);
    assert!(status.ok);
    let status: Value = serde_json::from_str(&status.out).unwrap();
    assert_eq!(status["games"], 1);
    assert!(run(&fixture, &["sync", "now", "cli-game"]).ok);
    let versions = fixture.rpc("sync.versions", json!({"id":"cli-game"}));
    let version = versions["result"]["versions"][0].as_str().unwrap();
    let listed = run(&fixture, &["sync", "versions", "cli-game"]);
    assert!(
        listed.ok && listed.out.contains(version),
        "CLI omitted uploaded version"
    );
    std::fs::write(&save, b"changed locally").unwrap();
    assert!(
        run(
            &fixture,
            &["sync", "restore", "cli-game", "--version", version]
        )
        .ok
    );
    assert_eq!(std::fs::read(&save).unwrap(), b"original\0\xff");
    std::fs::write(fixture.dir.join("fail"), ".zip").unwrap();
    std::fs::write(&save, b"must survive failed upload").unwrap();
    let failed = run(&fixture, &["sync", "now", "cli-game"]);
    assert!(!failed.ok, "failed upload returned success");
    assert_eq!(std::fs::read(save).unwrap(), b"must survive failed upload");
    fixture.shutdown();
}

/// 三条"用户直接看到的那行字"（BUG-5 / BUG-11 / BUG-14+37）—— 在此之前它们
/// 一条测试都没有，全靠手验。
#[test]
fn scan_list_and_reload_say_what_they_know() {
    let mut fixture = Fixture::new("cli-output");
    fixture.start();

    // 三个**同名、不同目录**的游戏：`add` 会给后两条加后缀，展示也该跟着加
    // —— `a&b` / `a—b` 归一化之后都是 `a-b`，从前三条会印成同一个 id（BUG-5）。
    let library = fixture.dir.join("library");
    for parent in ["a-b", "a&b", "a—b"] {
        let dir = library.join(parent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Game.exe"), b"fixture").unwrap();
    }
    let scan = run(&fixture, &["scan", library.to_str().unwrap()]);
    assert!(scan.ok, "{}", scan.err);
    let ids: Vec<String> = scan
        .out
        .lines()
        .filter_map(|line| line.trim().strip_prefix('['))
        .filter_map(|rest| rest.split(']').next())
        .map(str::to_string)
        .collect();
    assert_eq!(ids.len(), 3, "三个目录该有三条:\n{}", scan.out);
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "三条的 id 撞在一起了:{ids:?}");

    // 收进库里，再 `list` 两次：顺序要**一模一样**（BUG-37），而且两个开关都
    // 得印出来（BUG-14）—— `list` 直接读配置文件，不经过 daemon。
    assert!(run(&fixture, &["add", library.to_str().unwrap()]).ok);
    let first = run(&fixture, &["list"]);
    let second = run(&fixture, &["list"]);
    assert!(first.ok && second.ok);
    assert_eq!(first.out, second.out, "同一份配置两次 list 该一字不差");
    assert!(
        first.out.contains("watch:") && first.out.contains("direct:"),
        "两个开关都该印出来:\n{}",
        first.out
    );

    // `reload` 报的得是真的款数（BUG-11：从前永远印 0 款）。
    let reload = run(&fixture, &["reload"]);
    assert!(reload.ok, "{}", reload.err);
    assert!(reload.out.contains("3 款"), "该报 3 款:\n{}", reload.out);
    fixture.shutdown();
}

/// daemon 没起来时 `add` 照样能用（自己拿锁写盘）—— 用户不该为了加一个游戏先把
/// 守护进程拉起来。这也钉住"两条路都不改变用法"（BUG-16）。
#[test]
fn add_works_without_a_daemon_and_does_not_start_one() {
    let fixture = Fixture::new("cli-add-offline");
    let library = fixture.dir.join("离线 library");
    let game = library.join("Solo Game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("Game.exe"), b"fixture executable").unwrap();

    let reply = run(&fixture, &["add", library.to_str().unwrap()]);
    assert!(reply.ok, "{}", reply.err);
    assert!(reply.out.contains("Solo Game"), "{}", reply.out);

    let config = std::fs::read_to_string(fixture.dir.join("config.toml")).unwrap();
    assert!(
        config.contains("Solo Game"),
        "配置里没有新加的游戏:\n{config}"
    );
    // 写完之后那一发 reload 是尽力而为:它不该把 daemon 顺带拉起来。
    assert!(
        run(&fixture, &["status"]).out.contains("守护进程未响应"),
        "CLI 不该为了 add 启动守护进程"
    );
}

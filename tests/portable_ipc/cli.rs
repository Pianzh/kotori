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

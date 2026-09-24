//! 假工具的契约：未知参数和真实 I/O 错误必须失败，JSON 必须能表示特殊文件名。

use std::process::Command;

use serde_json::{Value, json};

use crate::fixture::Fixture;

fn command(fixture: &Fixture) -> Command {
    let mut command = Command::new(fixture.dir.join("bin/rclone"));
    for (key, value) in &fixture.envs {
        command.env(key, value);
    }
    command
}

#[test]
fn fake_rclone_rejects_unknown_commands_arguments_and_io_failures() {
    let mut fixture = Fixture::new("fake-errors");
    fixture.enable_fake_sync(false);
    for args in [
        vec!["unsupported"],
        vec!["copyto"],
        vec!["lsjson", "--typo", "kotori:bucket"],
        vec!["cat", "kotori:missing"],
        vec!["copyto", "kotori:missing", "kotori:destination"],
        vec!["deletefile", "kotori:missing"],
        vec!["cat", "kotori:../escape"],
    ] {
        let output = command(&fixture).args(&args).output().unwrap();
        assert!(!output.status.success(), "夹具错误地接受 {args:?}");
        assert!(!output.stderr.is_empty(), "失败必须有诊断: {args:?}");
    }
    assert!(!fixture.dir.join("destination").exists());
}

#[test]
fn fake_rclone_preserves_bytes_and_escapes_json_names() {
    let mut fixture = Fixture::new("fake-json");
    fixture.enable_fake_sync(false);
    let directory = fixture.dir.join("objects");
    std::fs::create_dir_all(directory.join("subdir")).unwrap();
    let filename = "中文 空格\"\\\n存档.dat";
    let content = b"\0binary\xff\n";
    std::fs::write(directory.join(filename), content).unwrap();
    let output = command(&fixture)
        .args(["lsjson", "--files-only", "kotori:objects"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["Name"], filename);
    assert_eq!(rows[0]["Size"], content.len());
    let copied = fixture.dir.join("copied");
    let output = command(&fixture)
        .args([
            "copyto",
            &format!("kotori:objects/{filename}"),
            copied.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(copied).unwrap(), content);
}

#[test]
fn an_injected_upload_failure_is_visible_over_ipc_and_keeps_local_data() {
    let mut fixture = Fixture::new("upload-failure");
    let remote = fixture.enable_fake_sync(true);
    fixture.start();
    let game = fixture.dir.join("game");
    let saves = game.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game.join("game.exe");
    std::fs::write(&exe, b"fixture").unwrap();
    std::fs::write(saves.join("progress.dat"), b"irreplaceable progress").unwrap();
    let response = fixture.rpc("game.create", json!({"name":"Failure", "exe_path":exe}));
    assert_eq!(response["result"]["id"], "failure", "{response}");
    assert_eq!(
        fixture.rpc(
            "game.update",
            json!({"id":"failure", "save_paths":["saves"], "auto_watch":false})
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
    std::fs::write(fixture.dir.join("fail"), ".zip").unwrap();
    let response = fixture.rpc("sync.now", json!({"id":"failure"}));
    assert_eq!(response["result"]["ok"], false, "{response}");
    assert_eq!(response["result"]["games"][0]["ok"], false, "{response}");
    assert!(
        response["result"]["games"][0]["error"].is_string(),
        "{response}"
    );
    assert_eq!(
        std::fs::read(saves.join("progress.dat")).unwrap(),
        b"irreplaceable progress"
    );
    assert!(crate::helpers::cloud_packages(&remote.join("games/failure")).is_empty());
}

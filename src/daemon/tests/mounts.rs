//! 挂载引用（盘号 + 磁盘内相对目录）在写接口上的语义。
//!
//! 用户 2026-09-25 定的口径：**显式给了就用它**、`null` = 不用了、键根本不出现 =
//! 自动识别一次；给了引用就**不再要求路径此刻存在**（盘可能插在别的机器上，也可能
//! 还没插）。"自动识别出什么"依赖真机的挂载表，那部分的确定性断言在 `crate::mount`
//! 自己的单测里，这里只钉住"谁说了算"。

use super::*;

use serde_json::{Value, json};

use crate::config::GameConfig;

/// 带一条 `demo` 档案的 daemon。配置**真的写进**临时文件：`mutate_config` 改之前
/// 会先重读磁盘，而"内存里有、盘上没有"的 daemon 在生产里不存在。
fn daemon_with_demo() -> (Daemon, PathBuf, PathBuf) {
    let dir = crate::config::test_scratch("daemon-mounts");
    let mut config = Config::default();
    config.games.insert(
        "demo".into(),
        GameConfig {
            cloud_id: None,
            game_dir_mount: None,
            exe_mount: None,
            exe_fingerprint: None,
            cloud_dir: None,
            cloud_rejected: Vec::new(),
            sync_enabled: true,
            cloud_conclusion: None,
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            save_paths: Vec::new(),
            wine_prefix: None,
            auto_watch: false,
            direct_launch: false,
            process_name: None,
            scale_profile: crate::config::ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        },
    );
    let path = dir.join("config.toml");
    crate::config::save_to(&path, &config).unwrap();
    (
        Daemon::new(config).with_config_path(path.clone()),
        path,
        dir,
    )
}

async fn call(daemon: &Daemon, method: &str, params: Value) -> Value {
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let reply = daemon.handle_request(&request.to_string()).await;
    serde_json::from_str(&reply.body).unwrap()
}

fn reference(relative: &str) -> Value {
    json!({ "disk": "AAAA-1111", "relative": relative })
}

#[tokio::test]
async fn an_explicit_reference_is_stored_and_the_absolute_path_gives_way() {
    let (daemon, path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.update",
        json!({
            "id": "demo",
            "exe_path": "",
            "exe_mount": reference("Games/demo/game.exe"),
        }),
    )
    .await;
    assert_eq!(value["result"]["success"], true, "{value}");

    let game = &crate::config::load_from(&path).unwrap().games["demo"];
    let mount = game.exe_mount.as_ref().expect("引用该被存下来");
    assert_eq!(mount.disk, "AAAA-1111");
    assert_eq!(mount.relative, PathBuf::from("Games/demo/game.exe"));
    assert!(
        game.exe_path.as_os_str().is_empty(),
        "引用是主存储，绝对路径该让位"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_reference_lets_a_path_that_is_not_mounted_here_through() {
    // 盘插在别的机器上时也得改得动 —— 这正是当初要加手填入口的理由。
    let (daemon, path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.update",
        json!({
            "id": "demo",
            "exe_path": "/media/other-machine/game.exe",
            "exe_mount": reference("Games/demo/game.exe"),
        }),
    )
    .await;
    assert_eq!(value["result"]["success"], true, "{value}");

    let game = &crate::config::load_from(&path).unwrap().games["demo"];
    assert!(game.exe_mount.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_null_reference_turns_it_off_and_is_not_filled_back_in() {
    let (daemon, path, dir) = daemon_with_demo();
    call(
        &daemon,
        "game.update",
        json!({
            "id": "demo",
            "exe_path": "",
            "exe_mount": reference("Games/demo/game.exe"),
        }),
    )
    .await;

    // 盘"回来了"：这一手给的绝对路径是**真实存在**的文件。用户明确说不要引用了，
    // 就不该在这同一笔里被自动识别又填回去。
    let exe = dir.join("game.exe");
    std::fs::write(&exe, b"exe").unwrap();
    let value = call(
        &daemon,
        "game.update",
        json!({ "id": "demo", "exe_path": exe, "exe_mount": null }),
    )
    .await;
    assert_eq!(value["result"]["success"], true, "{value}");

    let game = &crate::config::load_from(&path).unwrap().games["demo"];
    assert!(game.exe_mount.is_none(), "用户说不用了，就不是自动填回来");
    assert_eq!(game.exe_path, exe);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_path_that_is_not_here_is_still_refused_without_a_reference() {
    let (daemon, _path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.update",
        json!({ "id": "demo", "exe_path": "/media/other-machine/game.exe" }),
    )
    .await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("可执行文件不存在"),
        "{value}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_reference_that_escapes_its_disk_is_refused() {
    let (daemon, path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.update",
        json!({ "id": "demo", "exe_mount": { "disk": "AAAA-1111", "relative": "../escape" } }),
    )
    .await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("相对路径"),
        "{value}"
    );
    let game = &crate::config::load_from(&path).unwrap().games["demo"];
    assert!(game.exe_mount.is_none(), "被拒的引用不该留下半个");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn infer_says_nothing_about_a_path_that_is_not_there() {
    let (daemon, _path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "mount.infer",
        json!({ "path": dir.join("not-here") }),
    )
    .await;
    assert!(value["result"]["mount"].is_null(), "{value}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn creating_a_game_with_a_reference_does_not_need_the_file_to_be_here() {
    let (daemon, path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.create",
        json!({
            "name": "Mounted",
            "exe_path": "",
            "exe_mount": reference("Games/mounted/game.exe"),
        }),
    )
    .await;
    assert_eq!(value["result"]["name"], "Mounted", "{value}");

    let config = crate::config::load_from(&path).unwrap();
    let game = config
        .games
        .values()
        .find(|game| game.name == "Mounted")
        .expect("新建的档案该在配置里");
    assert!(game.exe_mount.is_some());
    assert!(game.exe_path.as_os_str().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn creating_a_game_without_a_reference_still_needs_a_real_exe() {
    let (daemon, _path, dir) = daemon_with_demo();
    let value = call(
        &daemon,
        "game.create",
        json!({ "name": "Ghost", "exe_path": "/media/other-machine/game.exe" }),
    )
    .await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("可执行文件不存在"),
        "{value}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

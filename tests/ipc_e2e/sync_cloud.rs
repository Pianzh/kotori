//! 云端有什么：列云端有哪些游戏，以及**第二台机器看得见第一台**。
//!
//! 这是"云同步身份"那件事的地基（先看得见，再谈哪一款对应哪一款）。与 `sync.rs`
//! 分开：那边管"存档怎么上去、怎么下来"，这边管"云端的**目录/标签**里有什么"。
//!
//! 假 rclone 把远端映射成一台机器上的目录树，所以"两台机器共用一个桶"就是让第二个
//! 夹具的假 rclone 指向第一个夹具的桶（[`Fixture::share_bucket_with`]）。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::{cloud_packages, rclone_calls};

/// 第二台机器要**看得见**第一台传上去的游戏 —— 这是"云同步身份"那件事的地基。
///
/// 从前 kotori 只会列**已知 id** 的版本（`games/<id>/`），从来没有列过 `games/`
/// 那一层：第二台机器于是不知道云端有什么，只有"两台机器给同一款游戏起的名字一字
/// 不差"时才能碰巧对上。
///
/// 这里的两台机器是**两份完全独立的 daemon**（各自的配置、数据目录、假 rclone），
/// 只有桶是同一个。B 的本机配置里根本没有这款游戏，它能说出"云端有哪几款、各有几版"
/// 才说明这条信息真的来自云端。
#[test]
fn a_second_machine_sees_what_the_first_one_uploaded() {
    let mut machine_a = Fixture::new("cloud-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();

    let credentials = json!({ "key_id": "id", "app_key": "key" });
    assert_eq!(
        machine_a.rpc("sync.set_credentials", credentials.clone())["result"]["stored"],
        true
    );

    // 桶里还什么都没有：一句空列表，不是一个报错。
    let empty = machine_a.rpc("sync.cloud_games", json!({}));
    assert_eq!(
        empty["result"]["games"].as_array().unwrap().len(),
        0,
        "{empty}"
    );

    // A 上一款游戏。
    let game_dir = machine_a.dir.join("SharedGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("save.dat"), b"from-a").unwrap();

    let created = machine_a.rpc(
        "game.create",
        json!({ "name": "Shared Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(created["result"]["id"], "shared-game", "{created}");
    machine_a.rpc(
        "game.update",
        json!({ "id": "shared-game", "save_paths": ["savedata"] }),
    );
    let uploaded = machine_a.rpc("sync.now", json!({ "id": "shared-game" }));
    assert_eq!(uploaded["result"]["ok"], true, "{uploaded}");
    assert_eq!(cloud_packages(&remote.join("games/shared-game")).len(), 1);

    // A 自己也列得出来（本地那一款与云端那一款是同一个 id）。
    let mine = machine_a.rpc("sync.cloud_games", json!({}));
    assert_eq!(mine["result"]["games"][0]["id"], "shared-game", "{mine}");
    assert_eq!(mine["result"]["games"][0]["versions"], 1, "{mine}");

    // --- 第二台机器：另一份 daemon，同一个桶 -------------------------------
    let mut machine_b = Fixture::new("cloud-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc("sync.set_credentials", credentials);

    // B 本机**没有**这款游戏（只有夹具自带的那一款）——所以下面那个 id 只可能
    // 是从云端读来的。
    let status = machine_b.rpc("sync.status", json!({}));
    let local_ids: Vec<&str> = status["result"]["games"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|game| game["id"].as_str())
        .collect();
    assert!(
        !local_ids.contains(&"shared-game"),
        "B 本机不该有这款游戏: {status}"
    );

    let cloud = machine_b.rpc("sync.cloud_games", json!({}));
    assert_eq!(cloud["result"]["games"][0]["id"], "shared-game", "{cloud}");
    assert_eq!(cloud["result"]["games"][0]["versions"], 1, "{cloud}");
    assert!(
        rclone_calls(&machine_b)
            .iter()
            .any(|call| call.starts_with("lsf --dirs-only")),
        "列云端游戏就该去问那一层目录: {:?}",
        rclone_calls(&machine_b)
    );

    // 而且 B 能顺着这个 id 拿到版本列表 —— 不是"列得出来但拿不到"。
    let versions = machine_b.rpc("sync.versions", json!({ "id": "shared-game" }));
    assert_eq!(
        versions["result"]["versions"].as_array().unwrap().len(),
        1,
        "{versions}"
    );
}

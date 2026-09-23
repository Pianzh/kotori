//! 云端有什么：列云端有哪些游戏，以及**第二台机器看得见第一台**。
//!
//! 这是"云同步身份"那件事的地基（先看得见，再谈哪一款对应哪一款）。与 `sync.rs`
//! 分开：那边管"存档怎么上去、怎么下来"，这边管"云端的**目录/标签**里有什么"。
//!
//! 假 rclone 把远端映射成一台机器上的目录树，所以"两台机器共用一个桶"就是让第二个
//! 夹具的假 rclone 指向第一个夹具的桶（[`Fixture::share_bucket_with`]）。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::{cloud_packages, field, rclone_calls};

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

/// 上传时认领的**云端身份**要落进配置，而且粘住。
///
/// 身份是"两台机器上哪两条档案是同一款游戏"的唯一答案（游戏名会重复、会不一样），
/// 所以它必须写在配置里、跨进程还在，而且第二次上传不会换一个。
#[test]
fn uploading_claims_a_cloud_identity_and_keeps_it() {
    let mut fixture = Fixture::new("identity");
    let remote = fixture.enable_fake_sync(true);
    fixture.start();

    let game_dir = fixture.dir.join("IdentityGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("save.dat"), b"first").unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Identity Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "identity-game", "{response}");
    fixture.rpc(
        "game.update",
        json!({ "id": "identity-game", "save_paths": ["savedata"] }),
    );
    fixture.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    // 还没上传过：配置里没有身份（也就没有"猜"的余地）。
    let before = std::fs::read_to_string(fixture.config.clone()).unwrap();
    assert!(!before.contains("cloud_id"), "{before}");

    let response = fixture.rpc("sync.now", json!({ "id": "identity-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");

    let written = std::fs::read_to_string(fixture.config.clone()).unwrap();
    let cloud_id = field(&written, "cloud_id").expect("上传之后应当认领一个身份");
    let machine_id = field(&written, "machine_id").expect("也要记下这是哪台机器");
    assert_eq!(cloud_id.len(), 36, "身份是个 uuid: {cloud_id}");
    assert_eq!(machine_id.len(), 36, "机器身份也是个 uuid: {machine_id}");

    // 再传一版：身份粘住，机器身份也不变。
    std::fs::write(saves.join("save.dat"), b"second").unwrap();
    let response = fixture.rpc("sync.now", json!({ "id": "identity-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");
    let again = std::fs::read_to_string(fixture.config.clone()).unwrap();
    assert_eq!(
        field(&again, "cloud_id").as_deref(),
        Some(cloud_id.as_str())
    );
    assert_eq!(
        field(&again, "machine_id").as_deref(),
        Some(machine_id.as_str())
    );
    assert_eq!(cloud_packages(&remote.join("games/identity-game")).len(), 2);

    // 包上也带着它 —— 取回之前比的就是这个（闸门的测试见下一条）。
    let packages = cloud_packages(&remote.join("games/identity-game"));
    // `cloud_packages` 给的就是文件名本身（带 .zip），别再加一次后缀。
    let zip = remote.join("games/identity-game").join(&packages[0]);
    let unpacked = fixture.dir.join("unpacked");
    let manifest = read_manifest(&zip, &unpacked);
    assert!(
        manifest.contains(&cloud_id),
        "包清单里要写着这是谁传的: {manifest}"
    );
}

/// 防错配闸：身份对不上时**一个文件都不铺**（自动取回与手动恢复两条路）。
///
/// 这是整件事里唯一不可逆的错误 —— 把另一款游戏的存档铺进本机这一款，用户下一次
/// 上传再把它推回云端（静默损坏存档）。这里用"改配置里的身份"来造这个局面：本机
/// 上传过一次（因此有了身份），然后把那份身份换成另一个。
#[test]
fn a_mismatched_identity_stops_both_pull_and_restore() {
    let mut fixture = Fixture::new("identity-gate");
    fixture.enable_fake_sync(true);
    let probe = fixture.enable_fake_display();
    fixture.start();

    let game_dir = fixture.dir.join("GateGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("save.dat"), b"from-cloud").unwrap();

    let response = fixture.rpc(
        "game.create",
        json!({ "name": "Gate Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(response["result"]["id"], "gate-game", "{response}");
    fixture.rpc(
        "game.update",
        json!({ "id": "gate-game", "save_paths": ["savedata"] }),
    );
    fixture.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let response = fixture.rpc("sync.now", json!({ "id": "gate-game" }));
    assert_eq!(response["result"]["ok"], true, "{response}");

    let original = std::fs::read_to_string(fixture.config.clone()).unwrap();
    let cloud_id = field(&original, "cloud_id").unwrap();

    // 把本机的身份换成另一个（模拟"云端那一版其实是别的一款"）。
    //
    // ⚠ 指纹也要一起改：启动前的自检（`sync::selfcheck`）会按指纹把身份**认回来**，
    // 所以"身份对不上"这条场景只有在**指纹也对不上**时才成立 —— 那正是换了一款游戏、
    // 而云端那条其实是别人的样子。
    std::fs::write(
        fixture.config.clone(),
        original
            .replace(&cloud_id, "11111111-2222-3333-4444-555555555555")
            .replace(
                &field(&original, "exe_fingerprint").unwrap(),
                "v1:1:0000000000000000000000000000000000000000000000000000000000000000",
            ),
    )
    .unwrap();
    assert_eq!(
        fixture.rpc("config.reload", json!({}))["result"]["success"],
        true
    );

    // 手动的「恢复」先试一次：身份对不上，不许覆盖本机存档。
    std::fs::write(saves.join("save.dat"), b"my own progress").unwrap();
    let response = fixture.rpc("sync.restore", json!({ "id": "gate-game" }));
    assert_eq!(response["result"]["ok"], false, "{response}");
    let error = response["result"]["game"]["error"].as_str().unwrap_or("");
    assert!(error.contains("另一个身份"), "{response}");
    assert_eq!(
        std::fs::read_to_string(saves.join("save.dat")).unwrap(),
        "my own progress",
        "拒绝就是拒绝：一个文件都不许动"
    );

    // 再走自动取回那条路（启动游戏之前）。
    std::fs::remove_dir_all(&saves).unwrap();
    let response = fixture.rpc("game.launch", json!({ "id": "gate-game" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("gamescope")),
        "假 gamescope 立刻退出: {response}"
    );
    assert!(probe.exists(), "启动那条路还是走到了 gamescope");
    assert!(
        !saves.join("save.dat").exists(),
        "身份对不上时绝不能把云端的存档铺下来"
    );

    // 界面看得见"为什么没铺"：状态里那一条取回记录带着原因。
    let status = fixture.rpc("sync.status", json!({}));
    let record = status["result"]["games"]
        .as_array()
        .unwrap()
        .iter()
        .find(|game| game["id"] == "gate-game")
        .cloned()
        .unwrap_or_default();
    assert!(
        record["last"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("另一个身份")),
        "状态里要说清为什么没取回: {status}"
    );

    // --- 把身份改回去：同一台机器的取回立刻恢复正常 -------------------------
    let tampered = std::fs::read_to_string(fixture.config.clone()).unwrap();
    std::fs::write(
        fixture.config.clone(),
        tampered.replace("11111111-2222-3333-4444-555555555555", &cloud_id),
    )
    .unwrap();
    assert_eq!(
        fixture.rpc("config.reload", json!({}))["result"]["success"],
        true
    );

    let response = fixture.rpc("game.launch", json!({ "id": "gate-game" }));
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("gamescope")),
        "假 gamescope 立刻退出: {response}"
    );
    assert_eq!(
        std::fs::read_to_string(saves.join("save.dat")).unwrap(),
        "from-cloud",
        "身份对得上时，启动前照样把云端的存档取回来"
    );
}

/// 解开一个包，返回它清单的原文（断言身份写在里面）。
fn read_manifest(zip: &std::path::Path, into: &std::path::Path) -> String {
    std::fs::create_dir_all(into).unwrap();
    let file = std::fs::File::open(zip).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut manifest = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("kotori-manifest.json").unwrap(),
        &mut manifest,
    )
    .unwrap();
    manifest
}

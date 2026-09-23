//! 配对：`sync.pairing` / `sync.pair` / `sync.reject` 的端到端。
//!
//! 与 `sync_cloud.rs` 分开：那边管"云端有什么、看不看得见"，这边管"云端那一条与本机
//! 哪一条档案是同一款"。规则本身在 `src/sync/pairing.rs`（纯函数），这里验的是它接上
//! 真实配置、真实桶之后的样子：自动绑、跟着对方的目录走、否掉之后不再绑回来。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::{cloud_packages, field};

/// 第二台机器**按 exe 指纹**认领第一款：配对扫描自动绑上，而且跟着对方的目录走。
///
/// 这是整件事的目的：两台机器给同一款游戏起的名字不一样，也能对上号；对上之后版本
/// 落在**同一个目录**里，于是互相看得见。名字一样不算数 —— 这条测试故意让两边的游戏
/// 名字与 slug 都不同。
#[test]
fn a_second_machine_pairs_by_fingerprint_and_follows_the_same_directory() {
    // 同一个 exe（内容一样 = 指纹一样），放在两台机器各自的目录里。
    let exe_bytes = b"\x7fELF kotori pairing probe".to_vec();

    let mut machine_a = Fixture::new("pair-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();

    let dir_a = machine_a.dir.join("Original Name");
    let saves_a = dir_a.join("savedata");
    std::fs::create_dir_all(&saves_a).unwrap();
    let exe_a = dir_a.join("game.exe");
    std::fs::write(&exe_a, &exe_bytes).unwrap();
    std::fs::write(saves_a.join("save.dat"), b"from-a").unwrap();

    let created = machine_a.rpc(
        "game.create",
        json!({ "name": "Original Name", "exe_path": exe_a, "game_dir": dir_a }),
    );
    assert_eq!(created["result"]["id"], "original-name", "{created}");
    machine_a.rpc(
        "game.update",
        json!({ "id": "original-name", "save_paths": ["savedata"] }),
    );
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let uploaded = machine_a.rpc("sync.now", json!({ "id": "original-name" }));
    assert_eq!(uploaded["result"]["ok"], true, "{uploaded}");
    assert_eq!(cloud_packages(&remote.join("games/original-name")).len(), 1);

    // --- 第二台机器：另一个名字、另一个 slug，但 exe 一样 ---------------------
    let mut machine_b = Fixture::new("pair-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    let dir_b = machine_b.dir.join("Renamed");
    let saves_b = dir_b.join("savedata");
    std::fs::create_dir_all(&saves_b).unwrap();
    let exe_b = dir_b.join("game.exe");
    std::fs::write(&exe_b, &exe_bytes).unwrap();
    let created = machine_b.rpc(
        "game.create",
        json!({ "name": "Renamed", "exe_path": exe_b, "game_dir": dir_b }),
    );
    assert_eq!(created["result"]["id"], "renamed", "{created}");
    machine_b.rpc(
        "game.update",
        json!({ "id": "renamed", "save_paths": ["savedata"] }),
    );

    // 扫描：指纹恰好命中一条，于是**自动绑上**（界面上那一行会写明依据）。
    let scan = machine_b.rpc("sync.pairing", json!({}));
    assert_eq!(scan["result"]["bound"], 1, "{scan}");
    let row = &scan["result"]["rows"][0];
    assert_eq!(row["state"], 1, "自动绑定: {scan}");
    assert_eq!(row["local_name"], "Renamed", "{scan}");
    assert_eq!(row["evidence"], "fingerprint", "{scan}");

    // 绑定落进配置：身份是 A 的，**云端落点也是 A 那个目录**。
    let written = std::fs::read_to_string(machine_b.config.clone()).unwrap();
    let a_config = std::fs::read_to_string(machine_a.config.clone()).unwrap();
    let a_cloud_id = field(&a_config, "cloud_id").unwrap();
    assert_eq!(
        field(&written, "cloud_id").as_deref(),
        Some(a_cloud_id.as_str())
    );
    assert_eq!(
        field(&written, "cloud_dir").as_deref(),
        Some("original-name"),
        "包要跟着身份放进同一个目录，否则两台机器永远互相看不见"
    );

    // 扫描顺带把**云端索引**写出来了：之后「云端存档」页读的就是它（一个桶一份），
    // 不必再读一遍每一张身份卡。
    let listed = machine_b.rpc("sync.cloud_list", json!({}));
    assert_eq!(listed["result"]["indexed"], true, "{listed}");
    let row = &listed["result"]["games"][0];
    assert_eq!(row["cloud_key"], "original-name", "{listed}");
    assert_eq!(row["name"], "Original Name", "云端记下的名字: {listed}");
    assert_eq!(row["local_id"], "renamed", "本机哪一条认了它: {listed}");

    // 配对之后，B 能取回 A 传的那一版（身份对得上，闸门放行）。
    let restored = machine_b.rpc("sync.restore", json!({ "id": "renamed" }));
    assert_eq!(restored["result"]["ok"], true, "{restored}");
    assert_eq!(
        std::fs::read_to_string(saves_b.join("save.dat")).unwrap(),
        "from-a",
        "配对之后，另一台机器传的存档取回来了"
    );

    // B 再上传一版：它落在 **A 的目录**里，所以这一款现在有两版。
    std::fs::write(saves_b.join("save.dat"), b"progress-on-b").unwrap();
    let uploaded = machine_b.rpc("sync.now", json!({ "id": "renamed" }));
    assert_eq!(uploaded["result"]["ok"], true, "{uploaded}");
    assert_eq!(
        cloud_packages(&remote.join("games/original-name")).len(),
        2,
        "B 的版本要与 A 的放在一起"
    );
    assert!(!remote.join("games/renamed").exists(), "不该另立一个目录");

    // 版本列表要按**云端落点**问（`sync.cloud_versions`）：这一款在本机的 id 是
    // `renamed`，而两个包都在 `original-name` 里 —— 这也正是"云端有、本机没有"的
    // 那类游戏唯一能列出版本的路（它们连本机 id 都没有）。
    let versions = machine_b.rpc("sync.cloud_versions", json!({ "key": "original-name" }));
    assert_eq!(
        versions["result"]["versions"].as_array().unwrap().len(),
        2,
        "{versions}"
    );

    // ⚠ 老的 `sync.versions` 收的是**本机 id**，在这台机器上会列到空目录里去。这不是
    // 意外，正是新 RPC 存在的理由（界面一律走上面那一条）；留着这条断言，是为了下次
    // 有人"顺手把两个统一一下"时立刻看见差异。
    let by_id = machine_b.rpc("sync.versions", json!({ "id": "renamed" }));
    assert_eq!(
        by_id["result"]["versions"].as_array().unwrap().len(),
        0,
        "{by_id}"
    );
}

/// exe 换了（就地打了补丁、换了版本）：指纹跟着换，**云端身份一个字不动**；换过之后
/// 另一台机器按新内容照样认得出这一款。
///
/// 这条钉住的是"指纹是'这个文件是什么'、身份是'这一款在云端是谁'"这条分界：指纹会变，
/// 身份不会 —— 换了版本还是同一款游戏，那是整个功能的立身之本。
#[test]
fn changing_the_exe_updates_the_fingerprint_but_not_the_cloud_identity() {
    let mut machine_a = Fixture::new("exe-change-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();

    let dir_a = machine_a.dir.join("Patched");
    let saves_a = dir_a.join("savedata");
    std::fs::create_dir_all(&saves_a).unwrap();
    let exe_a = dir_a.join("game.exe");
    std::fs::write(&exe_a, b"version one").unwrap();
    std::fs::write(saves_a.join("save.dat"), b"from-a").unwrap();
    machine_a.rpc(
        "game.create",
        json!({ "name": "Patched", "exe_path": exe_a, "game_dir": dir_a }),
    );
    machine_a.rpc(
        "game.update",
        json!({ "id": "patched", "save_paths": ["savedata"] }),
    );
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    assert_eq!(
        machine_a.rpc("sync.now", json!({ "id": "patched" }))["result"]["ok"],
        true
    );

    let before = std::fs::read_to_string(machine_a.config.clone()).unwrap();
    let cloud_id = field(&before, "cloud_id").expect("上传之后该有身份");
    let fingerprint = field(&before, "exe_fingerprint").expect("添加时就该算过指纹");

    // 就地换成另一份内容：**同一个路径**。用户走的是"改一下 exe"这条路，不是删了重建。
    std::fs::write(&exe_a, b"version two, patched").unwrap();
    let updated = machine_a.rpc(
        "game.update",
        json!({ "id": "patched", "exe_path": exe_a.to_string_lossy() }),
    );
    assert_eq!(updated["result"]["success"], true, "{updated}");

    let after = std::fs::read_to_string(machine_a.config.clone()).unwrap();
    assert_ne!(
        field(&after, "exe_fingerprint").as_deref(),
        Some(fingerprint.as_str()),
        "换了 exe，指纹必须跟着换: {after}"
    );
    assert_eq!(
        field(&after, "cloud_id").as_deref(),
        Some(cloud_id.as_str()),
        "身份是粘住的：换版本不是换游戏: {after}"
    );

    // 换过之后 A 再上传一版：身份卡里的指纹清单**跟着长**（老的那条留着，机器上装的
    // 是哪一份都可能遇到），身份本身还是原来那一条。
    let reuploaded = machine_a.rpc("sync.now", json!({ "id": "patched" }));
    assert_eq!(reuploaded["result"]["ok"], true, "{reuploaded}");

    // 另一台机器拿**新那份内容**：照样自动绑到同一条云端身份、同一个目录。
    let mut machine_b = Fixture::new("exe-change-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let dir_b = machine_b.dir.join("Renamed Patched");
    std::fs::create_dir_all(&dir_b).unwrap();
    let exe_b = dir_b.join("game.exe");
    std::fs::write(&exe_b, b"version two, patched").unwrap();
    machine_b.rpc(
        "game.create",
        json!({ "name": "Renamed Patched", "exe_path": exe_b, "game_dir": dir_b }),
    );
    let scan = machine_b.rpc("sync.pairing", json!({}));
    assert_eq!(scan["result"]["bound"], 1, "{scan}");
    let written = std::fs::read_to_string(machine_b.config.clone()).unwrap();
    assert_eq!(
        field(&written, "cloud_id").as_deref(),
        Some(cloud_id.as_str())
    );
    assert_eq!(field(&written, "cloud_dir").as_deref(), Some("patched"));
    assert!(
        !cloud_packages(&remote.join("games/patched")).is_empty(),
        "换了版本之后照样落在同一个目录里"
    );
}

/// 「不是同一款」要**记住**：下一次扫描不许再自动绑回来。
///
/// 没有这份记忆，用户否掉一次、界面下一次扫描又绑回去 —— 界面和他自己打架。
#[test]
fn rejecting_a_pairing_sticks() {
    let mut machine_a = Fixture::new("reject-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();

    let dir_a = machine_a.dir.join("Probe");
    std::fs::create_dir_all(&dir_a).unwrap();
    let exe = dir_a.join("game.exe");
    std::fs::write(&exe, b"same bytes everywhere").unwrap();
    machine_a.rpc(
        "game.create",
        json!({ "name": "Probe", "exe_path": exe, "game_dir": dir_a }),
    );
    machine_a.rpc("game.update", json!({ "id": "probe", "save_paths": ["."] }));
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    assert_eq!(
        machine_a.rpc("sync.now", json!({ "id": "probe" }))["result"]["ok"],
        true
    );
    let cloud_id = field(
        &std::fs::read_to_string(machine_a.config.clone()).unwrap(),
        "cloud_id",
    )
    .unwrap();

    // 第二台机器：指纹一样，于是扫描时会自动绑上。
    let mut machine_b = Fixture::new("reject-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let dir_b = machine_b.dir.join("Other Game");
    std::fs::create_dir_all(&dir_b).unwrap();
    let exe_b = dir_b.join("game.exe");
    std::fs::write(&exe_b, b"same bytes everywhere").unwrap();
    machine_b.rpc(
        "game.create",
        json!({ "name": "Other Game", "exe_path": exe_b, "game_dir": dir_b }),
    );
    machine_b.rpc(
        "game.update",
        json!({ "id": "other-game", "save_paths": ["."] }),
    );

    let scan = machine_b.rpc("sync.pairing", json!({}));
    assert_eq!(scan["result"]["bound"], 1, "{scan}");

    // 用户说：不是同一款。
    let rejected = machine_b.rpc(
        "sync.reject",
        json!({ "id": "other-game", "cloud_id": cloud_id }),
    );
    assert_eq!(rejected["result"]["ok"], true, "{rejected}");

    // 再扫一遍：不许再绑（连候选都不该出现）。
    let scan = machine_b.rpc("sync.pairing", json!({}));
    assert_eq!(scan["result"]["bound"], 0, "{scan}");
    let row = &scan["result"]["rows"][0];
    assert!(
        row["choices"].as_array().unwrap().is_empty(),
        "否掉过的不该再当候选: {scan}"
    );
    let written = std::fs::read_to_string(machine_b.config.clone()).unwrap();
    assert!(!written.contains("cloud_id"), "{written}");
    assert!(written.contains(&cloud_id), "要记下否掉了哪一个: {written}");

    // 而手动绑定照样可以（用户改主意了）。
    let paired = machine_b.rpc(
        "sync.pair",
        json!({ "id": "other-game", "cloud_key": "probe", "cloud_id": cloud_id }),
    );
    assert_eq!(paired["result"]["ok"], true, "{paired}");
    let written = std::fs::read_to_string(machine_b.config.clone()).unwrap();
    assert_eq!(
        field(&written, "cloud_id").as_deref(),
        Some(cloud_id.as_str())
    );
    assert_eq!(field(&written, "cloud_dir").as_deref(), Some("probe"));
    assert_eq!(cloud_packages(&remote.join("games/probe")).len(), 1);
}

/// 打开游戏前的自检：**只有当客户端答得上那一问时才问**，答过"关掉这一款"就不再问。
///
/// 这条路是全流程唯一会打断用户的地方（用户 2026-09-22："云同步（打开游戏）前必须自检"），
/// 所以两件事都要钉住：问得出来，以及**问过之后不再问**。
#[test]
fn the_pre_launch_check_asks_once_and_a_no_sticks() {
    let mut machine_a = Fixture::new("selfcheck-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();
    let dir_a = machine_a.dir.join("Cloud Game");
    let saves_a = dir_a.join("savedata");
    std::fs::create_dir_all(&saves_a).unwrap();
    let exe_a = dir_a.join("game.exe");
    std::fs::write(&exe_a, b"the real deal").unwrap();
    std::fs::write(saves_a.join("save.dat"), b"from-a").unwrap();
    machine_a.rpc(
        "game.create",
        json!({ "name": "Cloud Game", "exe_path": exe_a, "game_dir": dir_a }),
    );
    machine_a.rpc(
        "game.update",
        json!({ "id": "cloud-game", "save_paths": ["savedata"] }),
    );
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    assert_eq!(
        machine_a.rpc("sync.now", json!({ "id": "cloud-game" }))["result"]["ok"],
        true
    );
    assert_eq!(cloud_packages(&remote.join("games/cloud-game")).len(), 1);

    // 第二台机器：**exe 内容不一样**（重装过、打过补丁、或者压根是另一款），
    // 于是指纹认不出云端那一条 —— 这正是"要问一次"的那种情况。
    let mut machine_b = Fixture::new("selfcheck-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let dir_b = machine_b.dir.join("My Copy");
    let saves_b = dir_b.join("savedata");
    std::fs::create_dir_all(&saves_b).unwrap();
    let exe_b = dir_b.join("game.exe");
    std::fs::write(&exe_b, b"a different build").unwrap();
    std::fs::write(saves_b.join("save.dat"), b"my own progress").unwrap();
    machine_b.rpc(
        "game.create",
        json!({ "name": "My Copy", "exe_path": exe_b, "game_dir": dir_b }),
    );
    machine_b.rpc(
        "game.update",
        json!({ "id": "my-copy", "save_paths": ["savedata"] }),
    );

    // 老客户端（没声明 `selfcheck`）：照旧启动，绝不因为自检而点不动 ——
    // 而"认不出"这件事在这里等于"不自动取回"，本机存档一个字没动。
    let plain = machine_b.rpc("game.launch", json!({ "id": "my-copy" }));
    assert!(
        plain["result"].get("needs_sync_decision").is_none(),
        "没声明答得上那一问的客户端不该被拦: {plain}"
    );
    assert_eq!(
        std::fs::read_to_string(saves_b.join("save.dat")).unwrap(),
        "my own progress"
    );

    // 界面（答得上）：先不起游戏，把问题交出去。
    let ask = machine_b.rpc("game.launch", json!({ "id": "my-copy", "selfcheck": true }));
    assert_eq!(ask["result"]["needs_sync_decision"], true, "{ask}");
    assert!(ask["result"]["session_id"].is_null(), "{ask}");

    // 用户选了"关掉这一款的同步"。
    let resolved = machine_b.rpc("sync.resolve", json!({ "id": "my-copy", "choice": "off" }));
    assert_eq!(resolved["result"]["ok"], true, "{resolved}");
    let written = std::fs::read_to_string(machine_b.config.clone()).unwrap();
    assert!(written.contains("sync_enabled = false"), "{written}");
    assert!(written.contains("off:"), "要记住问过了: {written}");

    // 再点启动：**不再问**，直接起游戏（假 gamescope 立刻退出，所以是个错误回包，
    // 但那是"启动"那条路的事，不是"要决定"）。
    let again = machine_b.rpc("game.launch", json!({ "id": "my-copy", "selfcheck": true }));
    assert!(
        again["result"].get("needs_sync_decision").is_none(),
        "问过一次就不许再问: {again}"
    );
}

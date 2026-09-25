//! **一个桶一份的云端索引**：列云端只读它，添加游戏时也只问它。
//!
//! 从 `sync_cloud.rs` 拆出来（那边管"第二台机器看得见第一台""身份粘住"这些云端
//! 内容本身）。这里只管那份索引：它怎么来、读它便宜在哪、以及"添加游戏时云端有没有
//! 这一款"这一问（用户 2026-09-23 要的流程）。
//!
//! 索引是**镜像**：身份卡才是真相（丢了/坏了能照它重建）。所以这两条都在盯"读索引
//! 就够，一张身份卡都不用读"这件事。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::rclone_calls;

/// 「云端存档」页的数据来自**一个桶一份的索引**，不是遍历身份卡。
///
/// 这一条盯的是用户 2026-09-23 点出来的那个错误设计：从前"云端有哪些游戏、叫什么名字"
/// 要读每一张卡（kopia 那边读一张 = 一次 `restore`，200 款就是 200 趟）。现在上传时
/// 顺手把索引写出来，之后列云端只读索引。
#[test]
fn the_cloud_list_comes_from_one_index_not_from_every_card() {
    let mut machine_a = Fixture::new("index-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    // 桶里还没有索引：如实说"没建过"（界面据此提示去点一次深度扫描），不是"云端没有游戏"。
    let fresh = machine_a.rpc("sync.cloud_list", json!({}));
    assert_eq!(fresh["result"]["indexed"], false, "{fresh}");
    assert_eq!(
        fresh["result"]["games"].as_array().unwrap().len(),
        0,
        "{fresh}"
    );

    let game_dir = machine_a.dir.join("Indexed");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"\x7fELF index probe").unwrap();
    std::fs::write(saves.join("save.dat"), b"from-a").unwrap();

    let created = machine_a.rpc(
        "game.create",
        json!({ "name": "Indexed Game", "exe_path": exe, "game_dir": game_dir }),
    );
    assert_eq!(created["result"]["id"], "indexed-game", "{created}");
    machine_a.rpc(
        "game.update",
        json!({ "id": "indexed-game", "save_paths": ["savedata"] }),
    );
    assert_eq!(
        machine_a.rpc("sync.now", json!({ "id": "indexed-game" }))["result"]["ok"],
        true
    );

    // 上传成功之后索引就有了：名字是**云端记下的游戏名**，不是落点。
    let mine = machine_a.rpc("sync.cloud_list", json!({}));
    assert_eq!(mine["result"]["indexed"], true, "{mine}");
    let row = &mine["result"]["games"][0];
    assert_eq!(row["name"], "Indexed Game", "{mine}");
    assert_eq!(row["cloud_key"], "indexed-game", "{mine}");
    assert_eq!(row["versions"], 1, "{mine}");
    assert_eq!(row["local_id"], "indexed-game", "本机哪一条认了它: {mine}");
    assert!(
        row["exe_paths"][0]
            .as_str()
            .unwrap_or_default()
            .ends_with("game.exe"),
        "用过的 exe 路径只是个参考信息、也是搜索参数: {mine}"
    );
    assert!(remote.join("index").exists(), "索引该落在桶里");

    // --- 第二台机器：本机没有这款游戏，照样看得见（而且不读身份卡）-------------
    let mut machine_b = Fixture::new("index-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    let other = machine_b.rpc("sync.cloud_list", json!({}));
    assert_eq!(other["result"]["indexed"], true, "{other}");
    let row = &other["result"]["games"][0];
    assert_eq!(row["name"], "Indexed Game", "{other}");
    assert_eq!(row["versions"], 1, "{other}");
    assert_eq!(row["local_id"], "", "本机没有与它对上的档案: {other}");
    let calls = rclone_calls(&machine_b);
    assert!(
        !calls.iter().any(|call| call.contains("kotori-game.json")),
        "列云端只许读索引，一个身份卡都不许读: {calls:?}"
    );
}

/// 添加游戏时那一问（`sync.match`）：这个 exe 在云端是哪一款。
///
/// 用户 2026-09-23 定的流程：填完 exe 就认这一款，认出来就在本页直接确定。判据只有
/// **指纹**（名字两台机器可以不一样），而且这一问读的是索引 —— 不遍历身份卡。
/// 这里同时钉住三件事：命中要带回落点（认领要用它）、对不上就是 0 条、桶里还没索引
/// 时如实说"没建过"（那与"云端没有这一款"是两句话）。
#[test]
fn adding_a_game_asks_the_index_whether_the_cloud_already_has_it() {
    let mut machine_a = Fixture::new("match-a");
    let remote = machine_a.enable_fake_sync(true);
    machine_a.start();
    machine_a.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    let game_dir = machine_a.dir.join("Match");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe_bytes = b"\x7fELF match probe";
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, exe_bytes).unwrap();
    std::fs::write(saves.join("save.dat"), b"from-a").unwrap();

    machine_a.rpc(
        "game.create",
        json!({ "name": "Match Game", "exe_path": exe, "game_dir": game_dir }),
    );
    machine_a.rpc(
        "game.update",
        json!({ "id": "match-game", "save_paths": ["savedata"] }),
    );
    assert_eq!(
        machine_a.rpc("sync.now", json!({ "id": "match-game" }))["result"]["ok"],
        true
    );

    // 桶里还没建过索引时如实说"没建过"（界面据此提示去点一次深度扫描）——
    // 那与"云端没有这一款"是两句话。
    let mut machine_c = Fixture::new("match-c");
    machine_c.enable_fake_sync(true);
    machine_c.start();
    machine_c.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let fresh_exe = machine_c.dir.join("same.exe");
    std::fs::write(&fresh_exe, exe_bytes).unwrap();
    let fresh = machine_c.rpc("sync.match", json!({ "exe": fresh_exe }));
    assert_eq!(fresh["result"]["indexed"], false, "{fresh}");
    assert_eq!(
        fresh["result"]["games"].as_array().unwrap().len(),
        0,
        "{fresh}"
    );

    // --- 第二台机器：同一个 exe（字节相同 ⇒ 指纹相同），云端应当认得出 ----------
    let mut machine_b = Fixture::new("match-b");
    machine_b.enable_fake_sync(true);
    machine_b.share_bucket_with(&machine_a);
    machine_b.start();
    machine_b.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    let elsewhere = machine_b.dir.join("elsewhere.exe");
    std::fs::write(&elsewhere, b"\x7fELF nobody has this").unwrap();

    let same = machine_b.dir.join("same.exe");
    std::fs::write(&same, exe_bytes).unwrap();
    let hit = machine_b.rpc("sync.match", json!({ "exe": same }));
    assert_eq!(hit["result"]["indexed"], true, "{hit}");
    let games = hit["result"]["games"].as_array().unwrap();
    assert_eq!(games.len(), 1, "{hit}");
    assert_eq!(games[0]["name"], "Match Game", "{hit}");
    assert_eq!(
        games[0]["cloud_key"], "match-game",
        "认领要拿这个落点: {hit}"
    );
    assert_eq!(games[0]["local_id"], "", "本机还没有这一款: {hit}");
    assert!(
        hit["result"]["fingerprint"]
            .as_str()
            .unwrap_or_default()
            .starts_with("v1:"),
        "指纹由 daemon 现算: {hit}"
    );

    // 对不上的就是 0 条（不是猜一个最近的）。
    let miss = machine_b.rpc("sync.match", json!({ "exe": elsewhere }));
    assert_eq!(miss["result"]["indexed"], true, "{miss}");
    assert_eq!(
        miss["result"]["games"].as_array().unwrap().len(),
        0,
        "{miss}"
    );

    // 读不出指纹的文件（路径上没这个东西）要如实报错 —— 界面那一句是"没问成"，
    // 而不是"云端没有这一款"。
    let broken = machine_b.rpc(
        "sync.match",
        json!({ "exe": machine_b.dir.join("nope.exe") }),
    );
    assert!(broken.get("error").is_some(), "{broken}");

    let calls = rclone_calls(&machine_b);
    assert!(
        !calls.iter().any(|call| call.contains("kotori-game.json")),
        "匹配只许读索引，一个身份卡都不许读: {calls:?}"
    );
    assert!(remote.join("index").exists(), "索引该落在桶里");
}

/// 索引读到本机之后，**平时读它不再打网络**。
///
/// 用户 2026-09-23 的原话："为什么你每次读云端都要这么久，理论上来说不是应该把云端索引
/// 下载到本地再在本地查吗，自己选的页面默认不刷新直接读本地缓存。" 所以：
///   * 本地还没有缓存时 → 去云端读一次并把它存下来（`from_cache: false`）；
///   * 之后默认读 → 同一个结果，**一次 rclone 调用都不多**（`from_cache: true`）；
///   * `refresh: true`（「云端存档」页那颗刷新按钮）→ 真的再去云端。
///
/// ⚠ 用户 2026-09-24 把这口径钉得更死："只有按刷新键和每小时自动同步才会从云端更新本地
/// 索引，其他所有查询都只查本地索引" —— 所以**缓存旧了也不联网**（原来那条"读的时候发现
/// 超一小时就顺手刷一次"已经删掉），要新内容只有按刷新、或者等后台那一趟（起来先刷一次 +
/// 每小时）。
#[test]
fn the_cloud_list_is_read_from_a_local_cache_after_the_first_fetch() {
    let mut machine = Fixture::new("cache");
    machine.enable_fake_sync(true);
    machine.start();
    machine.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    // 桶里还没有索引，本机也还没有缓存 ⇒ 这一读必须去云端（并顺手把"没有索引"也记下来）。
    let first = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(first["result"]["indexed"], false, "{first}");
    assert_eq!(
        first["result"]["from_cache"], false,
        "第一次只能去云端: {first}"
    );
    let cached_at = first["result"]["cached_at"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!cached_at.is_empty(), "回包里要带'什么时候拿到的': {first}");

    // 第二次（默认）：还是那个结果，但**没有新的 rclone 调用** —— 读的是本机那份。
    let before = rclone_calls(&machine).len();
    let second = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(second["result"]["indexed"], false, "{second}");
    assert_eq!(second["result"]["from_cache"], true, "该读缓存了: {second}");
    assert_eq!(
        second["result"]["cached_at"], cached_at,
        "还是那一份（时间没变）: {second}"
    );
    assert_eq!(
        rclone_calls(&machine).len(),
        before,
        "第二次读不该打网络: {:?}",
        rclone_calls(&machine)
    );

    // 再读一次（「自己选…」浮层与添加页匹配走的也都是这条路）：仍然不打网络。
    let third = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(third["result"]["from_cache"], true, "{third}");
    assert_eq!(
        rclone_calls(&machine).len(),
        before,
        "读几次都该是本地: {:?}",
        rclone_calls(&machine)
    );

    // `refresh: true` 是那颗刷新按钮：真的再去云端，并且把缓存刷新（时间会变新）。
    let refreshed = machine.rpc("sync.cloud_list", json!({ "refresh": true }));
    assert_eq!(refreshed["result"]["from_cache"], false, "{refreshed}");
    assert!(
        rclone_calls(&machine).len() > before,
        "刷新按钮必须真的去云端: {:?}",
        rclone_calls(&machine)
    );
    // 上传成功之后缓存也跟着更新（这里没有游戏，用刷新这条路代表"我们刚知道最新内容"）。
    let after = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(after["result"]["from_cache"], true, "{after}");
}

#[test]
fn a_failed_refresh_keeps_the_old_cache_and_reports_the_error() {
    let mut machine = Fixture::new("cache-failure");
    machine.enable_fake_sync(true);
    machine.start();
    machine.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );

    let first = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(first["result"]["from_cache"], false, "{first}");
    let cached_at = first["result"]["cached_at"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    std::fs::write(machine.dir.join("fail"), "index").unwrap();
    let failed = machine.rpc("sync.cloud_list", json!({ "refresh": true }));
    assert_eq!(failed["result"]["from_cache"], true, "{failed}");
    assert_eq!(failed["result"]["cached_at"], cached_at, "{failed}");
    assert!(
        failed["result"]["refresh_error"].is_string(),
        "刷新失败必须把原因带回界面: {failed}"
    );

    std::fs::remove_file(machine.dir.join("fail")).unwrap();
    let recovered = machine.rpc("sync.cloud_list", json!({ "refresh": true }));
    assert_eq!(recovered["result"]["from_cache"], false, "{recovered}");
    assert!(recovered["result"]["refresh_error"].is_null(), "{recovered}");
}

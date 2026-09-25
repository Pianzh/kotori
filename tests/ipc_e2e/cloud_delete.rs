//! 「云端存档」的删除：删一版 / 清空这一款 / 删掉整条词条。
//!
//! 从 `sync_cloud.rs` 拆出来（那边答的是"云端有什么"，这边答的是"能不能删掉"）。
//! 三个操作都必须顺带把**索引**改对：详情页的版本列表是实时读桶的，而外层列表读的是
//! 本机缓存索引 —— 不跟着改，删完退回去还显示旧版数。

use serde_json::json;

use crate::fixture::Fixture;
use crate::helpers::cloud_packages;

/// 一款游戏在云端备好两版存档，返回（桶里那一款的目录，两版的名字，最旧在前）。
///
/// 三个删除操作都要先有东西可删，而"造出两版"这件事本身有点啰嗦（建游戏、配存档位置、
/// 传两次），抽出来省得每处抄一遍。
fn two_versions(
    machine: &mut Fixture,
    remote: &std::path::Path,
) -> (std::path::PathBuf, Vec<String>) {
    machine.rpc(
        "sync.set_credentials",
        json!({ "key_id": "id", "app_key": "key" }),
    );
    let game_dir = machine.dir.join("SharedGame");
    let saves = game_dir.join("savedata");
    std::fs::create_dir_all(&saves).unwrap();
    let exe = game_dir.join("game.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(saves.join("save.dat"), b"v1").unwrap();
    machine.rpc(
        "game.create",
        json!({ "name": "Shared Game", "exe_path": exe, "game_dir": game_dir }),
    );
    machine.rpc(
        "game.update",
        json!({ "id": "shared-game", "save_paths": ["savedata"] }),
    );
    let first = machine.rpc("sync.now", json!({ "id": "shared-game" }));
    assert_eq!(first["result"]["ok"], true, "{first}");
    // 内容改一下再传一次：两次的版本名是时间戳，本来就不同，但"内容真的变了"更像真的。
    std::fs::write(saves.join("save.dat"), b"v2").unwrap();
    let second = machine.rpc("sync.now", json!({ "id": "shared-game" }));
    assert_eq!(second["result"]["ok"], true, "{second}");

    let dir = remote.join("games/shared-game");
    let versions = machine.rpc("sync.cloud_versions", json!({ "key": "shared-game" }));
    let names: Vec<String> = versions["result"]["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 2, "{versions}");
    (dir, names)
}

/// 删掉云端某**一版**：别的版本和**词条**都不动，索引里的版数也跟着改。
///
/// ⚠ 索引那一半不能省：详情页的版本列表是实时读桶的（删完自然少一版），而外层列表读的
/// 是本机缓存索引 —— 不跟着改的话，删完退回去还显示"2 版"，要等一小时缓存过期才变。
#[test]
fn deleting_one_version_keeps_the_others_and_the_card() {
    let mut machine = Fixture::new("delete-one");
    let remote = machine.enable_fake_sync(true);
    machine.start();
    let (dir, names) = two_versions(&mut machine, &remote);
    assert_eq!(cloud_packages(&dir).len(), 2, "先要有两版");

    let deleted = machine.rpc(
        "sync.delete_version",
        json!({ "key": "shared-game", "version": names[0] }),
    );
    assert_eq!(deleted["result"]["ok"], true, "{deleted}");
    assert_eq!(deleted["result"]["left"], 1, "{deleted}");
    assert_eq!(cloud_packages(&dir).len(), 1, "云端只剩一版");

    // 词条没被碰：删存档和删词条是两件事。
    assert!(
        dir.join("kotori-game.json").exists(),
        "删一版不该动身份卡: {:?}",
        std::fs::read_dir(&dir).map(|it| it.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );

    // 剩的那一版还得能列出来，而且是刚才没删的那个。
    let left = machine.rpc("sync.cloud_versions", json!({ "key": "shared-game" }));
    assert_eq!(left["result"]["versions"][0]["name"], names[1], "{left}");

    // 索引：外层列表读的那一份也要跟上。
    machine.rpc("sync.cloud_list", json!({ "refresh": true }));
    let list = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(list["result"]["games"][0]["versions"], 1, "{list}");
    assert_eq!(list["result"]["games"][0]["latest"], names[1], "{list}");
}

/// 删**所有**存档：包全没了，但**词条留着** —— 下次同步还能往同一条身份传。
#[test]
fn clearing_the_versions_keeps_the_cloud_identity() {
    let mut machine = Fixture::new("delete-all");
    let remote = machine.enable_fake_sync(true);
    machine.start();
    let (dir, _) = two_versions(&mut machine, &remote);

    let cleared = machine.rpc("sync.delete_versions", json!({ "key": "shared-game" }));
    assert_eq!(cleared["result"]["ok"], true, "{cleared}");
    assert_eq!(cleared["result"]["removed"], 2, "{cleared}");
    assert_eq!(cloud_packages(&dir).len(), 0, "包应该全没了");
    assert!(
        dir.join("kotori-game.json").exists(),
        "清空存档不该动词条 —— 云端还要认得这一款"
    );

    machine.rpc("sync.cloud_list", json!({ "refresh": true }));
    let list = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(list["result"]["games"][0]["versions"], 0, "{list}");
    assert!(list["result"]["games"][0]["latest"].is_null(), "{list}");
}

/// 删**词条**：连存档一起抹掉，而且那一款要从「云端存档」那一页**整个消失**。
///
/// 最后那条是索引那一半：索引只会增/改、不会减，所以给条目加了个 `gone` 标志，
/// 列表跳过它。不加的话，删完那一款会以"还没有存档"的空壳留在列表里。
#[test]
fn deleting_the_identity_takes_the_saves_with_it() {
    let mut machine = Fixture::new("delete-identity");
    let remote = machine.enable_fake_sync(true);
    machine.start();
    let (dir, _) = two_versions(&mut machine, &remote);
    // 先把索引读进缓存，这样后面那次读的是本机缓存（删完不许还留着那一条）。
    machine.rpc("sync.cloud_list", json!({ "refresh": true }));

    let gone = machine.rpc("sync.delete_identity", json!({ "key": "shared-game" }));
    assert_eq!(gone["result"]["ok"], true, "{gone}");
    assert_eq!(gone["result"]["removed"], 2, "词条连存档一起删");
    assert_eq!(cloud_packages(&dir).len(), 0, "包应该全没了");
    assert!(
        !dir.join("kotori-game.json").exists(),
        "身份卡应该也没了: {:?}",
        std::fs::read_dir(&dir).map(|it| it.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );

    // 实时那条路（读身份卡）当然也列不出它了。
    let live = machine.rpc("sync.cloud_games", json!({}));
    assert_eq!(
        live["result"]["games"].as_array().unwrap().len(),
        0,
        "{live}"
    );

    // 而「云端存档」那一页读的是索引 —— 那一条要整个消失，不是留个 0 版的空壳。
    let list = machine.rpc("sync.cloud_list", json!({}));
    assert_eq!(
        list["result"]["games"].as_array().unwrap().len(),
        0,
        "词条删掉之后索引里那一条也要没了: {list}"
    );
}

/// 删一个云端**没有**的版本名：明确报错，不许静默成功。
///
/// ⚠ 这条盯的是 kopia：它底层的删除对找不到的版本是**静默成功**（那是给自动清理用的
/// best-effort 语义）。用户的显式删除要是走那条路，就成了"点了删除、其实什么也没删，
/// 界面还报成功"。
#[test]
fn deleting_a_version_the_cloud_never_had_is_refused() {
    let mut machine = Fixture::new("delete-missing");
    let remote = machine.enable_fake_sync(true);
    machine.start();
    let (dir, _) = two_versions(&mut machine, &remote);

    let refused = machine.rpc(
        "sync.delete_version",
        json!({ "key": "shared-game", "version": "20200101T000000Z" }),
    );
    assert!(refused["error"].is_object(), "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("没有这一版"),
        "{refused}"
    );
    assert_eq!(cloud_packages(&dir).len(), 2, "拒绝之后一版都不许少");
}

/// 版本名不合形状：**输入错误**，在碰网络之前就拒掉（与 `sync.restore` 同一套）。
#[test]
fn a_malformed_version_name_is_refused_before_any_network() {
    let mut machine = Fixture::new("delete-bad-name");
    let remote = machine.enable_fake_sync(true);
    machine.start();
    let (_, _) = two_versions(&mut machine, &remote);

    let bad = machine.rpc(
        "sync.delete_version",
        json!({ "key": "shared-game", "version": "昨天那一版" }),
    );
    assert!(bad["error"].is_object(), "{bad}");
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("不是合法的版本名"),
        "{bad}"
    );
}

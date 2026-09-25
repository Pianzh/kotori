//! 两种真实工具遵守同一套用户行为契约；仓库完全隔离。

use super::fixture::Fixture;
use serde_json::json;

fn round_trip(engine: &str) {
    let mut fixture = Fixture::new(engine);
    fixture.select_engine(engine);
    if engine == "rclone" {
        // 本地文件系统要求被列举目录存在；对象存储的空前缀没有此要求。
        for directory in ["games/contract", "index/main", "index/log"] {
            std::fs::create_dir_all(fixture.dir.join("bucket/fixture/saves").join(directory))
                .unwrap();
        }
    }
    fixture.start();
    let exe = fixture.game_exe();
    assert_eq!(
        fixture.rpc("game.create", json!({"name":"Contract", "exe_path":exe}))["result"]["id"],
        "contract"
    );
    assert_eq!(
        fixture.rpc(
            "game.update",
            json!({"id":"contract", "auto_watch":false, "save_paths":["saves"]})
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
    let saves = fixture.dir.join("saves");
    std::fs::create_dir_all(saves.join("nested")).unwrap();
    let path = saves.join("nested/中文 progress.dat");
    std::fs::write(&path, b"version one\0\xff").unwrap();
    std::fs::write(saves.join("empty"), b"").unwrap();
    let response = fixture.rpc("sync.now", json!({"id":"contract"}));
    assert_eq!(response["result"]["ok"], true, "{engine}: {response}");
    let response = fixture.rpc("sync.versions", json!({"id":"contract"}));
    let versions = response["result"]["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1, "{engine}: {response}");
    let first = versions[0].clone();
    std::fs::write(&path, b"version two").unwrap();
    let response = fixture.rpc("sync.now", json!({"id":"contract"}));
    assert_eq!(response["result"]["ok"], true, "{engine}: {response}");
    let response = fixture.rpc("sync.versions", json!({"id":"contract"}));
    assert_eq!(
        response["result"]["versions"].as_array().unwrap().len(),
        2,
        "{engine}: {response}"
    );
    let response = fixture.rpc("sync.restore", json!({"id":"contract", "version":first}));
    assert_eq!(response["result"]["ok"], true, "{engine}: {response}");
    assert_eq!(std::fs::read(&path).unwrap(), b"version one\0\xff");
    assert!(std::fs::read(saves.join("empty")).unwrap().is_empty());
    fixture.shutdown();
    fixture.start();
    let response = fixture.rpc("sync.versions", json!({"id":"contract"}));
    assert_eq!(
        response["result"]["versions"].as_array().unwrap().len(),
        2,
        "versions lost after restart: {response}"
    );
    // 云端落点在两个引擎上**叫法不同**：rclone 是目录名（= 游戏 id），kopia 是 `game:`
    // 标签值（= 身份 id，一个 UUID）。所以别硬写 `contract` —— 问云端一次，拿它给的 id
    // 当 key。⚠ 这正是 `PLAN-cloud-identity.md` §13.5 警告过的那件事：拿本机 id 当云端
    // 落点用。（rclone 那条恰好在两个 id 相同时是对的，所以只有 kopia 会红。）
    let key = fixture.rpc("sync.cloud_games", json!({}))["result"]["games"][0]["id"]
        .as_str()
        .expect("云端应该有这一款")
        .to_string();

    // ── 删一版：别的版本和**词条**都不动 ──────────────────────────────────
    let response = fixture.rpc(
        "sync.delete_version",
        json!({"key": &key, "version": first.clone()}),
    );
    assert_eq!(response["result"]["ok"], true, "{engine}: {response}");
    assert_eq!(response["result"]["left"], 1, "{engine}: {response}");
    let response = fixture.rpc("sync.versions", json!({"id":"contract"}));
    assert_eq!(
        response["result"]["versions"].as_array().unwrap().len(),
        1,
        "{engine}: {response}"
    );

    // ── 删词条：连存档一起，云端从此不认得这一款 ─────────────────────────
    let response = fixture.rpc("sync.delete_identity", json!({"key": &key}));
    assert_eq!(response["result"]["ok"], true, "{engine}: {response}");
    assert_eq!(response["result"]["removed"], 1, "{engine}: {response}");
    assert_eq!(
        fixture.rpc("sync.cloud_games", json!({}))["result"]["games"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "{engine}: 词条删掉之后云端不该还列得出这一款"
    );
    // 重启再看一次：不是"内存里没清干净"。
    fixture.shutdown();
    fixture.start();
    assert_eq!(
        fixture.rpc("sync.cloud_games", json!({}))["result"]["games"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "{engine}: 重启之后云端仍然不该有它"
    );
    fixture.shutdown();
}

#[test]
#[ignore = "CI 需安装真实 kopia 并设置 KOTORI_KOPIA"]
fn real_kopia_obeys_save_contract() {
    assert!(
        std::path::PathBuf::from(std::env::var_os("KOTORI_KOPIA").expect("KOTORI_KOPIA")).is_file()
    );
    round_trip("kopia");
}

#[test]
#[ignore = "CI 需安装真实 rclone 并设置 KOTORI_REAL_RCLONE"]
fn real_rclone_obeys_save_contract() {
    assert!(
        std::path::PathBuf::from(
            std::env::var_os("KOTORI_REAL_RCLONE").expect("KOTORI_REAL_RCLONE")
        )
        .is_file()
    );
    round_trip("rclone");
}

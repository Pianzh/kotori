//! `engine` 的单测。
//!
//! 参数表与 JSON 解析的测试在 `kopia_args` 里（纯函数，逐条断言）；这里管
//! **门面本身**：设置里选哪个引擎，跑起来就得是哪个——选错了在两个引擎的数据
//! 布局之间横跳，用户会看到"云端没有存档"，而且不会报错。

use std::path::PathBuf;

use super::*;
use crate::config::SyncEngine;

fn settings(engine: SyncEngine) -> SyncConfig {
    SyncConfig {
        enabled: true,
        engine,
        bucket: "kotori-saves".to_string(),
        ..SyncConfig::default()
    }
}

#[test]
fn the_backend_is_whichever_engine_the_settings_name() {
    let rclone = Backend::with_binary(
        PathBuf::from("/bin/true"),
        settings(SyncEngine::Rclone),
        Keyring::memory(),
    );
    assert!(
        rclone.describe().starts_with("rclone"),
        "{}",
        rclone.describe()
    );

    let kopia = Backend::with_binary(
        PathBuf::from("/bin/true"),
        settings(SyncEngine::Kopia),
        Keyring::memory(),
    );
    assert!(
        kopia.describe().starts_with("kopia"),
        "{}",
        kopia.describe()
    );
}

// ── 真 kopia 的端到端验收 ────────────────────────────────────────────────
//
// 用**本地目录仓库**跑完整闭环（上传 → 再传一版 → 取回旧版 → 删除），不碰 B2、
// 不联网。命令本身是真的：kopia 各子命令的行为（多路径会拍成多个快照、
// `snapshot delete` 默认只演练、stdout 才是干净 JSON）都是靠它验出来的，
// 换成假引擎就什么都验不到。
//
// 标记 `#[ignore]`：CI 里没有 kopia。本地验收：
//   KOTORI_KOPIA=/path/to/kopia cargo test --bin kotori -- --ignored kopia

use std::path::Path;
use std::time::Duration;

use crate::sync::SaveTarget;

fn real_kopia() -> PathBuf {
    std::env::var_os("KOTORI_KOPIA")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .expect("设 KOTORI_KOPIA 指向 kopia 可执行文件（本地验收用）")
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kotori-kopia-{}-{}-{}",
        tag,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn target(local: &Path, key: &str) -> SaveTarget {
    SaveTarget {
        key: key.to_string(),
        configured: key.to_string(),
        local: local.to_path_buf(),
        exclude: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "需要真 kopia：设 KOTORI_KOPIA 指向它再跑 --ignored"]
async fn a_real_kopia_repository_round_trips_a_save() {
    let dir = temp("live");
    let engine =
        kopia::Kopia::with_binary(real_kopia(), settings(SyncEngine::Kopia), Keyring::memory())
            .with_home(dir.join("home"))
            .with_local_repository(dir.join("repo"));

    let saves = dir.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    std::fs::write(saves.join("save01.sav"), "v1").unwrap();
    let target = target(&saves, "savedata");
    let timeout = Duration::from_secs(120);

    // 第一版：一次快照，落到本地仓库里。
    let stamp1 = "20260901T000000000Z-aaaa1111";
    let work = dir.join("work1");
    std::fs::create_dir_all(&work).unwrap();
    let report = engine
        .send(
            "demo",
            stamp1,
            std::slice::from_ref(&target),
            None,
            &work,
            timeout,
        )
        .await
        .unwrap();
    assert_eq!(report.locations, vec!["savedata"]);
    assert_eq!(report.entries.len(), 1);
    assert_eq!(engine.versions("demo").await.unwrap(), vec![stamp1]);

    // 第二版：改了存档内容再拍一次。
    std::fs::write(saves.join("save01.sav"), "v2").unwrap();
    let stamp2 = "20260902T000000000Z-bbbb2222";
    let work2 = dir.join("work2");
    std::fs::create_dir_all(&work2).unwrap();
    engine
        .send(
            "demo",
            stamp2,
            std::slice::from_ref(&target),
            None,
            &work2,
            timeout,
        )
        .await
        .unwrap();
    assert_eq!(
        engine.versions("demo").await.unwrap(),
        vec![stamp1, stamp2],
        "两个版本，最旧在前"
    );

    // 取回第一版：内容必须是那一刻的，而且清单读得出来。
    let fetched = dir.join("fetched");
    let manifest = engine
        .fetch("demo", stamp1, &fetched, timeout)
        .await
        .unwrap();
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(manifest.entries[0].key, "savedata");
    assert_eq!(
        std::fs::read_to_string(fetched.join("savedata").join("save01.sav")).unwrap(),
        "v1",
        "回退拿到的就是那一刻的那一版"
    );

    // 保留窗口：`snapshot delete` 少了 `--delete` 只会演练，这一步就是在盯它。
    engine.remove("demo", stamp1).await.unwrap();
    assert_eq!(
        engine.versions("demo").await.unwrap(),
        vec![stamp2],
        "删除要真的生效，不是 dry-run"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
#[ignore = "需要真 kopia：设 KOTORI_KOPIA 指向它再跑 --ignored"]
async fn a_real_kopia_repository_keeps_two_games_apart() {
    let dir = temp("live-tags");
    let engine =
        kopia::Kopia::with_binary(real_kopia(), settings(SyncEngine::Kopia), Keyring::memory())
            .with_home(dir.join("home"))
            .with_local_repository(dir.join("repo"));
    let timeout = Duration::from_secs(120);

    for (game, body) in [("one", "first"), ("two", "second")] {
        let saves = dir.join(format!("saves-{game}"));
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), body).unwrap();
        let work = dir.join(format!("work-{game}"));
        std::fs::create_dir_all(&work).unwrap();
        engine
            .send(
                game,
                "20260901T000000000Z-aaaa1111",
                &[target(&saves, "savedata")],
                None,
                &work,
                timeout,
            )
            .await
            .unwrap();
    }

    // 快照的 source 是**本机绝对路径**，两台机器上根本对不上 —— 分得开靠的是
    // 快照上的 `game:` 标签，不是路径。
    assert_eq!(
        engine.versions("one").await.unwrap(),
        vec!["20260901T000000000Z-aaaa1111"]
    );
    assert_eq!(
        engine.versions("two").await.unwrap(),
        vec!["20260901T000000000Z-aaaa1111"],
        "另一个游戏有它自己的一版，不会串台"
    );

    let fetched = dir.join("fetched");
    engine
        .fetch("two", "20260901T000000000Z-aaaa1111", &fetched, timeout)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(fetched.join("savedata").join("save.sav")).unwrap(),
        "second"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 另一台机器要用**它自己的** kopia 配置看见这台机器传的版本。
///
/// kopia 的"我是谁"（用户名 + 主机名）是**建/连仓库那一刻烧进配置**的 —— 2026-09-22
/// 实测 0.22.3：事后改主机名再跑，快照仍然记成配置里那个名字，`os.Hostname()` 不管用。
/// 所以"第二台机器"在这里就是"另一份配置 + 另一个主机名"，不必真开一台机器。
///
/// ⚠ 这条**验不出**"少写 `-a` 会看不见"：0.22.3 上不带 `-a` 也能列出别的 source
/// （`snapshot list` 不带 `<source>` 参数时压根不筛），所以 `-a` 今天是保险。它把
/// "另一台机器看得见"这件事本身钉住 —— 这正是这台机器上真会坏的那一半。
#[tokio::test]
#[ignore = "需要真 kopia：设 KOTORI_KOPIA 指向它再跑 --ignored"]
async fn a_real_kopia_repository_is_visible_from_another_machine() {
    let dir = temp("live-two-hosts");
    let timeout = Duration::from_secs(120);
    let repo = dir.join("repo");

    // 机器 A：正常那台，上一款游戏的一版。
    let a = kopia::Kopia::with_binary(real_kopia(), settings(SyncEngine::Kopia), Keyring::memory())
        .with_home(dir.join("home-a"))
        .with_local_repository(repo.clone());
    let saves = dir.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    std::fs::write(saves.join("save01.sav"), "v1").unwrap();
    let work = dir.join("work-a");
    std::fs::create_dir_all(&work).unwrap();
    a.send(
        "demo",
        "20260901T000000000Z-aaaa1111",
        &[target(&saves, "savedata")],
        None,
        &work,
        timeout,
    )
    .await
    .unwrap();

    // 机器 B：另一份配置，同一个仓库。
    let b = kopia::Kopia::with_binary(real_kopia(), settings(SyncEngine::Kopia), Keyring::memory())
        .with_home(dir.join("home-b"))
        .with_local_repository(repo);
    b.check().await.unwrap();
    rewrite_hostname(
        &dir.join("home-b").join("repository.config"),
        "another-machine",
    );

    assert_eq!(
        b.versions("demo").await.unwrap(),
        vec!["20260901T000000000Z-aaaa1111"],
        "B 要看得见 A 拍的版本"
    );
    assert_eq!(
        b.cloud_games().await.unwrap(),
        vec![CloudGame {
            id: "demo".to_string(),
            versions: 1
        }],
        "也要说得清云端有哪几款、各有几版"
    );
    // B 自己一版都没拍过：上面那两条只可能是从仓库里读来的。
    assert_eq!(
        a.versions("demo").await.unwrap(),
        vec!["20260901T000000000Z-aaaa1111"]
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 把一份 kopia 配置的主机名改成别的机器。
fn rewrite_hostname(config: &Path, hostname: &str) {
    let text = std::fs::read_to_string(config).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        json.get("hostname").is_some(),
        "kopia 配置里没有 hostname 字段，这条测试的前提变了: {text}"
    );
    json["hostname"] = serde_json::Value::String(hostname.to_string());
    std::fs::write(config, serde_json::to_string(&json).unwrap()).unwrap();
}

//! 云端索引的单测：一个桶一份、写读闭环、并发不丢条目。
//!
//! 全部跑在假 rclone 上（桶就是磁盘上一个目录），所以"两台机器共用一个桶"就是两个
//! `Runner` 指着同一个夹具 —— 真机上那件事（同时同步）在这里可以逐条摆出来。

use crate::sync::index::{CloudIndex, IndexBundle, IndexGame, delta_name, is_delta_name};
use crate::sync::index_main_path;
use crate::sync::runner::testing::FakeRclone;

fn entry(cloud_id: &str, key: &str, name: &str, machine: &str, fingerprint: &str) -> IndexGame {
    let mut identity = crate::sync::cloud::GameIdentity::new(cloud_id, name);
    identity.merge_machine(crate::sync::cloud::MachineIdentity {
        machine_id: machine.to_string(),
        label: format!("host-{machine}"),
        fingerprints: vec![fingerprint.to_string()],
        locations: vec!["rel-savedata".to_string()],
        exe_paths: vec![format!("/games/{key}/game.exe")],
    });
    IndexGame::from_identity(key, identity)
}

/// 桶里那份合并快照（直接读磁盘，绕开引擎，方便断言"到底写了什么"）。
fn main_on_disk(fake: &FakeRclone) -> Option<CloudIndex> {
    let path = fake.bucket_path(&index_main_path(&fake.settings(0)));
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).unwrap())
}

#[tokio::test]
async fn an_index_that_was_never_written_reads_as_nothing_at_all() {
    let fake = FakeRclone::new("index-empty");
    let runner = fake.runner(0);

    // "桶里还没有索引"必须与"云端一款都没有"分得开：界面据此提示去深扫一次。
    assert!(runner.read_index().await.unwrap().is_none());
    // 什么改动都没有时不该往桶里写东西（不然每次同步都多两个对象）。
    runner.update_index("machine-a", Vec::new()).await.unwrap();
    assert!(
        !fake.calls().iter().any(|call| call.contains("copyto")),
        "空改动不该往桶里写东西: {:?}",
        fake.calls()
    );
    assert!(main_on_disk(&fake).is_none());
}

#[tokio::test]
async fn one_machine_writes_the_index_and_another_reads_it_without_touching_any_card() {
    let fake = FakeRclone::new("index-roundtrip");
    let machine_a = fake.runner(0);

    machine_a
        .update_index(
            "machine-a",
            vec![entry("c1", "one", "云端那款", "machine-a", "v1:10:aa")],
        )
        .await
        .unwrap();

    // 合并快照写了、增量也写了，而且增量被记进了 merged（否则会每次重读一遍）。
    let main = main_on_disk(&fake).expect("该写下合并快照");
    assert_eq!(main.len(), 1);
    assert_eq!(main.merged.len(), 1, "{:?}", main.merged);
    assert!(is_delta_name(&main.merged[0]), "{:?}", main.merged);
    assert_eq!(main.games[0].identity.name, "云端那款");

    // 第二台机器（同一个桶、另一个 Runner）：读得回来，一个字都没读身份卡。
    let machine_b = fake.runner(0);
    let read = machine_b.read_index().await.unwrap().expect("索引该在");
    assert_eq!(read.len(), 1);
    assert_eq!(read.games[0].cloud_key, "one");
    assert!(read.games[0].identity.has_fingerprint("v1:10:aa"));
    let calls = fake.calls();
    assert!(
        !calls.iter().any(|call| call.contains("kotori-game.json")),
        "读索引不许碰身份卡: {calls:?}"
    );
}

#[tokio::test]
async fn two_machines_writing_at_once_never_lose_an_entry() {
    let fake = FakeRclone::new("index-race");
    let machine_a = fake.runner(0);
    let machine_b = fake.runner(0);

    machine_a
        .update_index(
            "machine-a",
            vec![entry("c1", "one", "甲传的", "machine-a", "v1:10:aa")],
        )
        .await
        .unwrap();
    // 记下 A 写完之后的合并快照：下面要把它"回滚"成这一刻的样子，重现并发覆盖。
    let after_a = main_on_disk(&fake).unwrap();

    machine_b
        .update_index(
            "machine-b",
            vec![entry("c2", "two", "乙传的", "machine-b", "v1:20:bb")],
        )
        .await
        .unwrap();
    let after_both = main_on_disk(&fake).unwrap();
    assert_eq!(after_both.len(), 2);

    // ⚠ 这就是"两台机器同时同步"的那一下：B 的合并快照被 A 的写入盖掉了，
    // 桶里只剩 A 那一版（`merged` 里没有 B 的增量）。
    let text = serde_json::to_string(&after_a).unwrap();
    fake.put(&index_main_path(&fake.settings(0)), &text);

    // 读的时候必须**仍然**看得见乙传的那一款：它躺在自己那条增量里。
    let read = fake
        .runner(0)
        .read_index()
        .await
        .unwrap()
        .expect("索引该在");
    assert_eq!(read.len(), 2, "被覆盖掉的条目要从增量里捡回来");
    assert!(has(&read, "c1") && has(&read, "c2"));

    // 下一次写入会把它并回合并快照，并记进 `merged` —— 之后就只需读一份了。
    let machine_a = fake.runner(0);
    let merged = machine_a
        .update_index(
            "machine-a",
            vec![entry("c3", "three", "丙传的", "machine-a", "v1:30:cc")],
        )
        .await
        .unwrap();
    assert_eq!(merged.len(), 3);
    let main = main_on_disk(&fake).unwrap();
    assert_eq!(main.len(), 3);
    assert!(main.merged.len() >= 3, "{:?}", main.merged);
}

#[tokio::test]
async fn a_stray_object_in_the_log_directory_is_never_read() {
    let fake = FakeRclone::new("index-stray");
    let runner = fake.runner(0);
    runner
        .update_index(
            "machine-a",
            vec![entry("c1", "one", "一", "machine-a", "v1:10:aa")],
        )
        .await
        .unwrap();

    // 桶是用户的：`log/` 里塞了别的东西（别人放的、或者我们将来改格式留下的）一律不碰。
    let log = crate::sync::index_log_path(&fake.settings(0));
    fake.put(&format!("{log}/notes.json"), "{\"hello\":\"world\"}");
    fake.put(
        &format!("{log}/machine-z-20260923T101500123Z.json"),
        "not json at all",
    );

    let read = runner.read_index().await.unwrap().unwrap();
    assert_eq!(read.len(), 1, "乱七八糟的对象不该进来");
    assert!(has(&read, "c1"));
}

#[tokio::test]
async fn the_union_keeps_what_each_machine_knows_about_the_same_game() {
    let fake = FakeRclone::new("index-union");
    let machine_a = fake.runner(0);
    machine_a
        .update_index(
            "machine-a",
            vec![entry("c1", "one", "同一款", "machine-a", "v1:10:aa")],
        )
        .await
        .unwrap();

    // 乙这台机器对**同一款游戏**补一个新指纹（换过 exe）：不能把甲的指纹抹掉。
    let machine_b = fake.runner(0);
    machine_b
        .update_index(
            "machine-b",
            vec![entry("c1", "one", "同一款", "machine-b", "v1:20:bb")],
        )
        .await
        .unwrap();

    let read = fake.runner(0).read_index().await.unwrap().unwrap();
    assert_eq!(read.len(), 1, "同一个身份只该有一条");
    let card = &read.games[0].identity;
    assert!(card.has_fingerprint("v1:10:aa"), "{card:?}");
    assert!(card.has_fingerprint("v1:20:bb"), "{card:?}");
    assert_eq!(card.machines.len(), 2);
    assert_eq!(
        card.machines
            .iter()
            .flat_map(|machine| machine.exe_paths.clone())
            .count(),
        2,
        "两台机器各自的 exe 路径都留着（只作参考信息）"
    );
    // 摘要以**这一次**为准（谁最后写谁说了算：它读过了并集）。
    assert_eq!(read.games[0].versions, 0);
}

#[tokio::test]
async fn a_delta_name_that_carries_a_slash_never_lands_in_the_bucket() {
    // 名义上是个防御性断言：增量名由我们拼（机器 id + 时间戳），不许出现路径分隔符。
    let name = delta_name("1a2b", &crate::sync::index::stamp());
    assert!(!name.contains('/') && !name.contains('\\'), "{name}");
    assert!(is_delta_name(&name));
}

/// [`IndexBundle`] 的合并视图与"该读哪几条"是纯逻辑，这里顺手钉一下边界。
#[test]
fn the_bundle_view_is_the_union_of_the_snapshot_and_its_deltas() {
    let mut main = CloudIndex::new();
    main.merge(entry("c1", "one", "一", "machine-a", "v1:10:aa"));
    let delta = CloudIndex::delta(vec![entry("c2", "two", "二", "machine-b", "v1:20:bb")]);
    let bundle = IndexBundle {
        main: Some(main),
        deltas: vec![("machine-b-20260923T101500123Z.json".to_string(), delta)],
    };
    assert_eq!(bundle.merged_view().len(), 2);
    assert!(!bundle.is_empty());
    assert!(delta_name("1a2b", "20260923T101500123Z").starts_with("1a2b-"));
}

/// 索引里有没有这个身份。
fn has(index: &CloudIndex, cloud_id: &str) -> bool {
    index
        .games
        .iter()
        .any(|game| game.identity.cloud_id == cloud_id)
}

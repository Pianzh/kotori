//! `index` 的单元测试：并集读写、增量名、格式判定。
//!
//! 从 `index.rs` 拆出来 —— 那边连着测试一起数越过了 500 行的硬线（AGENTS.md）。

use super::*;

fn machine(id: &str, prints: &[&str], paths: &[&str]) -> MachineIdentity {
    MachineIdentity {
        machine_id: id.to_string(),
        label: format!("host-{id}"),
        fingerprints: prints.iter().map(|p| p.to_string()).collect(),
        locations: vec!["rel-savedata".to_string()],
        parents: Vec::new(),
        exe_paths: paths.iter().map(|p| p.to_string()).collect(),
    }
}

fn game(cloud_id: &str, key: &str, name: &str, machine: MachineIdentity) -> IndexGame {
    let mut identity = GameIdentity::new(cloud_id, name);
    identity.merge_machine(machine);
    IndexGame::from_identity(key, identity)
}

#[test]
fn a_delta_is_a_whole_index_without_any_merge_history() {
    let delta = CloudIndex::delta(vec![game("c1", "one", "一", machine("a", &["f1"], &[]))]);
    assert_eq!(delta.format, INDEX_FORMAT);
    assert!(delta.merged.is_empty(), "增量自己没并过谁");
    assert_eq!(delta.len(), 1);
}

#[test]
fn merging_keeps_one_entry_per_identity_and_lets_the_newer_one_win() {
    let mut union = CloudIndex::new();
    let mut older = game("c1", "one", "旧名字", machine("a", &["f1"], &[]));
    older.updated = "2026-09-23T10:00:00Z".to_string();
    union.merge(older);

    // 旧的不许盖掉新的。
    let mut stale = game("c1", "one", "陈年副本", machine("b", &["f0"], &[]));
    stale.updated = "2026-09-22T10:00:00Z".to_string();
    union.merge(stale);
    assert_eq!(union.len(), 1);
    assert_eq!(
        union.games[0].identity.name, "旧名字",
        "{}",
        union.games[0].identity.name
    );

    // 新的可以。
    let mut newer = game("c1", "one", "新名字", machine("b", &["f2"], &[]));
    newer.updated = "2026-09-23T11:00:00Z".to_string();
    union.merge(newer);
    assert_eq!(union.len(), 1);
    assert_eq!(union.games[0].identity.name, "新名字");

    // 另一款就是另一条。
    union.merge(game("c2", "two", "另一款", machine("a", &["f9"], &[])));
    assert_eq!(union.len(), 2);
}

/// 两台机器同时给同一款写增量：后到的那份若整条胜出，就会把对面那台机器的
/// 指纹与位置抹掉（BUG-24）。机器是"一台一条"的集合，只并集。
#[test]
fn a_later_write_does_not_erase_the_other_machines_record() {
    // b 在 a 之后写，但它读到的是更早的并集（没看见 a）⇒ 更新的那份里只有 b。
    let mut union = CloudIndex::new();
    let mut from_a = game(
        "c1",
        "one",
        "同名",
        machine("a", &["fa"], &["D:/A/game.exe"]),
    );
    from_a.updated = "2026-09-23T10:00:00Z".to_string();
    union.merge(from_a);

    let mut from_b = game(
        "c1",
        "one",
        "同名",
        machine("b", &["fb"], &["D:/B/game.exe"]),
    );
    from_b.updated = "2026-09-23T11:00:00Z".to_string();
    union.merge(from_b);

    let mut ids: Vec<&str> = union.games[0]
        .identity
        .machines
        .iter()
        .map(|m| m.machine_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["a", "b"], "两台机器都该在:{ids:?}");

    // 反过来的到达顺序（旧的那份后到）同样不能抹掉新的那台。
    let mut union = CloudIndex::new();
    let mut newer = game("c1", "one", "同名", machine("b", &["fb"], &[]));
    newer.updated = "2026-09-23T11:00:00Z".to_string();
    union.merge(newer);
    let mut older = game("c1", "one", "同名", machine("a", &["fa"], &[]));
    older.updated = "2026-09-23T10:00:00Z".to_string();
    union.merge(older);

    let mut ids: Vec<&str> = union.games[0]
        .identity
        .machines
        .iter()
        .map(|m| m.machine_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["a", "b"], "两台机器都该在:{ids:?}");
}

#[test]
fn a_fingerprint_finds_its_entry_and_says_how_many_it_found() {
    let mut index = CloudIndex::new();
    index.merge(game("c1", "one", "一", machine("a", &["f1"], &[])));
    index.merge(game("c2", "two", "二", machine("b", &["f2"], &[])));
    // 指纹相同却分属两个身份：两台机器没能认出彼此（各建了身份）。要问，不许猜。
    index.merge(game("c3", "three", "三", machine("c", &["f2"], &[])));

    let hits = index.by_fingerprint("f1");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].cloud_key, "one");
    assert_eq!(index.by_fingerprint("f2").len(), 2, "多个命中要全都说出来");
    assert!(index.by_fingerprint("f9").is_empty(), "对不上就是没有");
}

#[test]
fn absorbing_deltas_reports_what_it_swallowed() {
    let mut main = CloudIndex::new();
    main.merge(game("c1", "one", "一", machine("a", &["f1"], &[])));
    let names = main.absorb(vec![
        (
            "machine-b-20260923T100000Z.json".to_string(),
            CloudIndex::delta(vec![game("c2", "two", "二", machine("b", &["f2"], &[]))]),
        ),
        // 空增量也照样记账：它确实被读过了，没必要下次再读一遍。
        (
            "machine-c-20260923T100001Z.json".to_string(),
            CloudIndex::delta(Vec::new()),
        ),
    ]);
    assert_eq!(names.len(), 2);
    assert_eq!(main.len(), 2);
    assert!(main.games.iter().any(|game| game.identity.cloud_id == "c2"));
}

#[test]
fn a_bundle_merges_its_deltas_and_remembers_what_it_swallowed() {
    let mut main = CloudIndex::new();
    let mut settled = game("c1", "one", "一", machine("a", &["f1"], &[]));
    settled.updated = "2026-09-23T10:00:00Z".to_string();
    main.merge(settled);
    assert!(main.needs("machine-b-2026.json"));

    let bundle = IndexBundle {
        main: Some(main),
        deltas: vec![(
            "machine-b-2026.json".to_string(),
            CloudIndex::delta(vec![game("c2", "two", "二", machine("b", &["f2"], &[]))]),
        )],
    };
    let union = bundle.merged_view();
    assert_eq!(union.len(), 2, "增量也要算进来");
    assert!(!bundle.is_empty());

    // 并过的增量就不用再读了 —— 这条判据同时是"清理只许删这些"的依据。
    let mut after = bundle.main.clone().unwrap();
    after.mark_merged("machine-b-2026.json");
    assert!(!after.needs("machine-b-2026.json"));
    assert!(after.needs("machine-c-2026.json"), "没记着的一律当还没并");

    // 桶里一次都没写过时是"空"，区分得开"云端没有游戏"与"索引还没建"。
    assert!(IndexBundle::default().is_empty());
    assert_eq!(IndexBundle::default().merged_view().len(), 0);
}

#[test]
fn every_timestamp_is_the_same_width_so_string_order_is_time_order() {
    let a = "2026-09-23T09:59:59Z";
    let b = "2026-09-23T10:00:00Z";
    assert_eq!(a.len(), b.len(), "等宽是字典序=时间序的前提");
    assert!(a < b);
    let written = now();
    assert_eq!(written.len(), a.len(), "{written}");
    assert!(written.ends_with('Z'), "{written}");
    assert_eq!(
        delta_name("1a2b", "20260923T100000Z"),
        "1a2b-20260923T100000Z.json"
    );
    // 增量名会当文件名/目录名用：不许有冒号（Windows 上非法）。
    let fresh = stamp();
    assert!(!fresh.contains(':'), "{fresh}");
    assert!(is_delta_name(&delta_name("1a2b", &fresh)), "{fresh}");
    // 不是我们写的东西一律不认（桶是用户的）。
    for bad in [
        "",
        "notes.json",
        "1a2b.json",
        "1a2b-2026.json",
        "1a2b-xxx.json",
    ] {
        assert!(!is_delta_name(bad), "{bad}");
    }
}

#[test]
fn an_unknown_format_version_is_not_guessed_at() {
    let text = serde_json::to_string(&CloudIndex::new()).unwrap();
    assert_eq!(
        serde_json::from_str::<CloudIndex>(&text).unwrap().format,
        INDEX_FORMAT
    );
    // 未来版本：读得进来但格式号不认识 —— 上层据此当"读不懂"，不当"能用"。
    let future = r#"{"format":99,"updated":"2026-09-23T10:00:00Z","games":[]}"#;
    assert_ne!(
        serde_json::from_str::<CloudIndex>(future).unwrap().format,
        INDEX_FORMAT
    );
    // 少字段的索引也要读得进来（`games` / `merged` 缺了当空，不猜）。
    let sparse = r#"{"format":1,"updated":"2026-09-23T10:00:00Z"}"#;
    let parsed: CloudIndex = serde_json::from_str(sparse).unwrap();
    assert!(parsed.games.is_empty() && parsed.merged.is_empty());
    // `updated` 是结构字段，缺了就是坏文件 —— 不当成"空索引"（那会让它永远赢不了合并）。
    assert!(serde_json::from_str::<CloudIndex>(r#"{"format":1}"#).is_err());
}

/// `is_supported()` 是三条读取路径共同的判据（BUG-25）：自己写的格式认，别的
/// 一律不认 —— 上面那条只证明了"格式号读得出来"，这一条证明"读出来之后怎么判"。
#[test]
fn only_our_own_format_counts_as_supported() {
    assert!(CloudIndex::new().is_supported());
    let future = r#"{"format":99,"updated":"2026-09-23T10:00:00Z","games":[]}"#;
    assert!(
        !serde_json::from_str::<CloudIndex>(future)
            .unwrap()
            .is_supported()
    );
}

/// 名字里有多字节字符时**不许 panic**（BUG-26）：19 个字节、末字节是 `Z`，而第
/// 15 个字节正好落在某个两字节字符的内部 —— 从前 `at[..15]` 就是在这里炸的，
/// 一条异常对象名足以打断整个索引读取。
#[test]
fn a_multibyte_delta_name_is_rejected_instead_of_panicking() {
    let at = format!("{}abZ", "é".repeat(8));
    assert_eq!(at.len(), 19, "构造要正好 19 字节");
    let name = format!("1a2b-{at}.json");
    assert!(!is_delta_name(&name), "{name}");
}

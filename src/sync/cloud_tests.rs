//! `cloud` 的单元测试：身份卡、机器合并、指纹认人。
//!
//! 从 `cloud.rs` 拆出来 —— 理由同上，500 行是硬线（AGENTS.md）。

use super::*;

#[test]
fn directory_listings_become_usable_names() {
    let output = "3days/\nlife-game/\n\nlong-name/\n3days/\n";
    assert_eq!(
        parse_dirs(output),
        vec![
            "3days".to_string(),
            "life-game".to_string(),
            "long-name".to_string()
        ],
        "尾斜杠要去掉，空行与重复项都不留"
    );
    assert!(parse_dirs("").is_empty(), "云端还没有游戏时是空的");
    // 没带尾斜杠也认（老 rclone、或者有人 `--dir-slash=false`）。
    assert_eq!(parse_dirs("solo\n"), vec!["solo".to_string()]);
}

fn identity(cloud_id: &str) -> PackIdentity {
    PackIdentity {
        cloud_id: cloud_id.to_string(),
        machine_id: Some("machine-1".to_string()),
        fingerprint: None,
        locations: vec!["rel-savedata".to_string()],
    }
}

#[test]
fn only_the_same_identity_may_touch_local_saves() {
    // 同一个身份：唯一一种敢动本机存档的情形。
    assert_eq!(
        identity_match(Some("abc"), Some(&identity("abc"))),
        IdentityMatch::Same
    );

    // 两个身份不一样 = 云端那一版是别的一款。这是**静默损坏存档**那条路，
    // 必须拒绝，而且说清楚"本机一个都没动"。
    let mismatch = identity_match(Some("abc"), Some(&identity("xyz")));
    assert_eq!(
        mismatch,
        IdentityMatch::Different {
            cloud: "xyz".to_string(),
            local: "abc".to_string()
        }
    );
    let refusal = mismatch.refusal().unwrap();
    assert!(refusal.contains("已跳过"), "{refusal}");
    assert!(refusal.contains("本机存档一个都没动"), "{refusal}");
    // 只露前 8 位：身份是 uuid，界面上不必看全。
    assert!(!refusal.contains("xyz1234567890"), "{refusal}");

    // 缺一头都不猜：本机还没认领身份 / 云端那一版没有身份段。
    for (local, remote, cloud_has) in [
        (None, Some(identity("abc")), true),
        (Some("abc"), None, false),
        (None, None, false),
    ] {
        let verdict = identity_match(local, remote.as_ref());
        assert_eq!(
            verdict,
            IdentityMatch::Unpaired {
                cloud_has_identity: cloud_has
            }
        );
        let refusal = verdict.refusal().unwrap();
        assert!(refusal.contains("已跳过"), "{refusal}");
    }
    assert!(IdentityMatch::Same.refusal().is_none());
}

#[test]
fn an_identity_survives_the_round_trip_into_a_manifest() {
    let identity = PackIdentity {
        cloud_id: "8f2c-…".to_string(),
        machine_id: Some("1a2b".to_string()),
        fingerprint: Some("v1:184320000:9f3c".to_string()),
        locations: vec!["rel-savedata".to_string(), "win-appdata_game".to_string()],
    };
    let text = serde_json::to_string(&identity).unwrap();
    assert_eq!(
        serde_json::from_str::<PackIdentity>(&text).unwrap(),
        identity
    );
    // 老包（没有身份段）读出来是 `None`，不是"解析失败"。
    assert_eq!(
        serde_json::from_str::<Option<PackIdentity>>("null").unwrap(),
        None
    );
    assert_eq!(short_id("8f2c1234567890", 8), "8f2c1234");
    assert_eq!(
        short_id("8f2c1234567890", 6),
        "8f2c12",
        "目录名后缀只要 6 位"
    );
}

fn machine(machine_id: &str, prints: &[&str]) -> MachineIdentity {
    MachineIdentity {
        machine_id: machine_id.to_string(),
        label: format!("host-{machine_id}"),
        fingerprints: prints.iter().map(|p| p.to_string()).collect(),
        locations: vec!["rel-savedata".to_string()],
        parents: Vec::new(),
        exe_paths: vec![format!("/games/{machine_id}/game.exe")],
    }
}

#[test]
fn an_identity_keeps_one_entry_per_machine_and_only_appends() {
    let mut identity = GameIdentity::new("cloud-1", "示例游戏");
    assert_eq!(identity.format, IDENTITY_FORMAT);
    assert!(identity.machines.is_empty());

    identity.merge_machine(machine("machine-a", &["v1:10:aa"]));
    identity.merge_machine(machine("machine-b", &["v1:20:bb"]));
    assert_eq!(identity.machines.len(), 2);

    // 同一台机器再来一次：**只追加**，不重复、不覆盖别人的。
    let mut again = machine("machine-a", &["v1:10:aa", "v1:30:cc"]);
    again.label = "renamed-host".to_string();
    identity.merge_machine(again);
    assert_eq!(identity.machines.len(), 2, "一台机器一条");
    assert_eq!(identity.machines[0].label, "renamed-host", "机器名会变");
    assert_eq!(
        identity.machines[0].fingerprints,
        vec!["v1:10:aa".to_string(), "v1:30:cc".to_string()],
        "重复的不再写一遍，新的追加在后面"
    );
    assert_eq!(
        identity.machines[1].fingerprints,
        vec!["v1:20:bb".to_string()],
        "别人的指纹一个都没动"
    );

    // 用过的 exe 路径与指纹同一条规矩：只追加，不重复，也不动别人的。
    let mut moved = machine("machine-a", &["v1:10:aa"]);
    moved.exe_paths = vec![
        "/games/machine-a/game.exe".to_string(),
        "/mnt/games/elsewhere/game.exe".to_string(),
    ];
    identity.merge_machine(moved);
    assert_eq!(
        identity.machines[0].exe_paths,
        vec![
            "/games/machine-a/game.exe".to_string(),
            "/mnt/games/elsewhere/game.exe".to_string()
        ],
        "新路径追加在后面，已有的不再写一遍"
    );
    assert_eq!(identity.machines[1].exe_paths.len(), 1);
}

#[test]
fn a_fingerprint_finds_the_identity_it_belongs_to() {
    let mut one = GameIdentity::new("cloud-1", "one");
    one.merge_machine(machine("machine-a", &["v1:10:aa"]));
    let mut two = GameIdentity::new("cloud-2", "two");
    two.merge_machine(machine("machine-b", &["v1:20:bb"]));
    let all = vec![
        ("games/one".to_string(), one),
        ("games/two".to_string(), two),
    ];

    let hits = GameIdentity::find_by_fingerprint(&all, "v1:20:bb");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1.cloud_id, "cloud-2");
    // 键要跟着回来：rclone 那边就是"包放哪个目录"。
    assert_eq!(hits[0].0, "games/two");

    // 认不出来就是 0 个 —— "找不到就新建"，绝不硬凑一个。
    assert!(GameIdentity::find_by_fingerprint(&all, "v1:99:zz").is_empty());
    // 两张卡都报同一个指纹（复制过存档目录、或者建了两条档案）：**要问**。
    let mut three = GameIdentity::new("cloud-3", "three");
    three.merge_machine(machine("machine-c", &["v1:20:bb"]));
    let ambiguous = vec![
        ("games/two".to_string(), all[1].1.clone()),
        ("games/three".to_string(), three),
    ];
    assert_eq!(
        GameIdentity::find_by_fingerprint(&ambiguous, "v1:20:bb").len(),
        2
    );
}

#[test]
fn an_identity_card_round_trips_and_tolerates_missing_optional_fields() {
    let mut identity = GameIdentity::new("8f2c", "示例游戏");
    identity.merge_machine(machine("1a2b", &["v1:184320000:9f3c"]));
    let text = serde_json::to_string_pretty(&identity).unwrap();
    assert_eq!(
        serde_json::from_str::<GameIdentity>(&text).unwrap(),
        identity
    );
    // 绝不放绝对路径与内容：整张卡里只该有身份、机器名、指纹、位置 key。
    assert!(!text.contains("/home/"), "{text}");
    assert!(text.contains("rel-savedata"), "{text}");

    // 手写的、或者将来少字段的卡也要读得进来（缺的按空处理，不猜）。
    let sparse = r#"{"format":1,"cloud_id":"c1"}"#;
    let parsed: GameIdentity = serde_json::from_str(sparse).unwrap();
    assert_eq!(parsed.machines.len(), 0);
    assert_eq!(parsed.cloud_id, "c1");
}

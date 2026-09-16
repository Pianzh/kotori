//! `archive` 的单测：打包 → 清单 → 解包 → 合并判定。
//!
//! 这些测试全部在临时目录里跑，不碰网络、不碰 rclone。最要紧的是最后两条：
//! "本机更新过的文件不会被旧包盖掉"和"本机独有的文件不会被删"——那是 ADR-012
//! 的命根子，从前由 `rclone --update` 保证，现在由我们的 [`plan`] 保证。

use std::path::{Path, PathBuf};

use super::pack::excluded_by;
use super::unpack::{parse_manifest, read_manifest};
use super::*;
use crate::sync::SaveTarget;

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kotori-archive-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn target(key: &str, local: &Path) -> SaveTarget {
    SaveTarget {
        key: key.to_string(),
        configured: key.to_string(),
        local: local.to_path_buf(),
        exclude: Vec::new(),
    }
}

fn write(dir: &Path, relative: &str, body: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn entry(key: &str, path: &str, size: u64, mtime_ms: i64) -> Entry {
    Entry {
        key: key.to_string(),
        path: path.to_string(),
        size,
        mtime_ms,
    }
}

fn manifest(entries: Vec<Entry>) -> Manifest {
    Manifest {
        format: FORMAT,
        created: "2026-09-15T12:00:00Z".to_string(),
        locations: Vec::new(),
        entries,
    }
}

#[test]
fn a_packed_save_comes_back_with_its_contents_and_its_times() {
    let dir = temp("roundtrip");
    let saves = dir.join("saves");
    write(&saves, "save01.sav", "one");
    write(&saves, "nested/save02.sav", "two");
    let zip = dir.join("v.zip");

    let report = pack(&zip, &[target("rel-savedata", &saves)], now()).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.locations, vec!["rel-savedata".to_string()]);
    assert!(report.missing.is_empty());
    assert_eq!(report.entries[0].path, "nested/save02.sav");
    assert_eq!(report.entries[0].size, 3);

    let into = dir.join("out");
    let manifest = extract(&zip, &into).unwrap();
    assert_eq!(manifest, read_manifest(&zip).unwrap());
    assert_eq!(manifest.created, "2026-09-15T12:00:00Z");
    assert_eq!(
        std::fs::read_to_string(into.join("rel-savedata/save01.sav")).unwrap(),
        "one"
    );
    assert_eq!(
        std::fs::read_to_string(into.join("rel-savedata/nested/save02.sav")).unwrap(),
        "two"
    );
    // 时间按清单盖回去：zip 自带的时间戳只有 2 秒精度，用它判"谁新"会在往返
    // 一次之后失真——这正是"取回云端新存档"这条路最怕的事。
    let packed = into.join("rel-savedata/save01.sav");
    let restored = mtime_ms(&std::fs::metadata(&packed).unwrap());
    assert_eq!(restored, manifest.entries[1].mtime_ms);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn packing_reports_locations_that_are_not_on_this_machine() {
    let dir = temp("missing");
    let present = dir.join("here");
    std::fs::create_dir_all(&present).unwrap();
    let zip = dir.join("v.zip");

    let report = pack(
        &zip,
        &[
            target("rel-here", &present),
            target("win-appdata", &dir.join("elsewhere")),
        ],
        now(),
    )
    .unwrap();

    // 位置在、但一个文件都没有：也要算进这一版，否则恢复时会被当成"云端没有"。
    assert_eq!(report.locations, vec!["rel-here".to_string()]);
    assert_eq!(report.missing, vec!["win-appdata".to_string()]);
    assert!(report.entries.is_empty());

    let manifest = read_manifest(&zip).unwrap();
    assert!(manifest.has_location("rel-here"));
    assert!(!manifest.has_location("win-appdata"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn exclusions_still_work_without_rclone() {
    // 从前这条规则是 `rclone --exclude` 的活，改成一版一包之后必须自己保证。
    let patterns = vec!["*.log".to_string(), "cache/".to_string(), "  ".to_string()];
    assert!(excluded_by(&patterns, "debug.log"));
    assert!(excluded_by(&patterns, "nested/deep/debug.log"));
    assert!(!excluded_by(&patterns, "save01.sav"));
    assert!(!excluded_by(&patterns, "log/debug.txt"));
    // 带 `/` 的模式匹配整条路径，`*` 不跨分隔符。
    assert!(excluded_by(&["logs/*".to_string()], "logs/x.txt"));
    assert!(!excluded_by(&["logs/*".to_string()], "logs/sub/x.txt"));
    // 空白模式不是"排除一切"。
    assert!(!excluded_by(&patterns, "save02.sav"));

    let dir = temp("exclude");
    let saves = dir.join("saves");
    write(&saves, "save01.sav", "1");
    write(&saves, "debug.log", "noise");
    let mut target = target("rel-savedata", &saves);
    target.exclude = vec!["*.log".to_string()];
    let zip = dir.join("v.zip");

    let report = pack(&zip, &[target], now()).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.excluded, 1);
    assert_eq!(report.entries[0].path, "save01.sav");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_automatic_pull_never_overwrites_a_newer_local_save() {
    let dir = temp("newer");
    let saves = dir.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    write(&saves, "older-than-cloud.sav", "local");
    write(&saves, "newer-than-cloud.sav", "local");
    write(&saves, "only-local.sav", "local");

    let local_meta = std::fs::metadata(saves.join("newer-than-cloud.sav")).unwrap();
    let cloud = manifest(vec![
        // 云端更新：该取。
        entry("rel-savedata", "older-than-cloud.sav", 99, i64::MAX),
        // 本机更新：不许动（这就是"上一次上传失败，本机才是最新的"那一幕）。
        entry("rel-savedata", "newer-than-cloud.sav", 1, 1),
        // 本机没有：取回来。
        entry("rel-savedata", "absent-locally.sav", 5, i64::MAX),
    ]);
    assert!(mtime_ms(&local_meta) > 1);

    let merged = plan(&cloud, &[target("rel-savedata", &saves)], Merge::Newer).unwrap();
    let taken: Vec<&str> = merged.take.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(taken, vec!["older-than-cloud.sav", "absent-locally.sav"]);
    assert_eq!(merged.kept.len(), 1);
    assert_eq!(merged.kept[0].path, "newer-than-cloud.sav");

    // 本机独有的文件只被列出来，绝不删。
    assert_eq!(
        merged.extras,
        vec!["rel-savedata/only-local.sav".to_string()]
    );
    let empty = plan(
        &manifest(Vec::new()),
        &[target("rel-savedata", &saves)],
        Merge::Newer,
    )
    .unwrap();
    assert_eq!(
        empty.extras,
        vec![
            "rel-savedata/newer-than-cloud.sav".to_string(),
            "rel-savedata/older-than-cloud.sav".to_string(),
            "rel-savedata/only-local.sav".to_string(),
        ]
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_same_file_written_twice_is_compared_by_size() {
    let dir = temp("same");
    let saves = dir.join("saves");
    write(&saves, "save.sav", "12345");
    let meta = std::fs::metadata(saves.join("save.sav")).unwrap();
    let stamp = mtime_ms(&meta);

    // 时间一样、大小一样 ⇒ 没必要再写一遍。
    let same = manifest(vec![entry("rel-savedata", "save.sav", 5, stamp)]);
    let merged = plan(&same, &[target("rel-savedata", &saves)], Merge::Newer).unwrap();
    assert!(merged.take.is_empty());
    assert_eq!(merged.kept.len(), 1);

    // 时间一样、大小不同 ⇒ 云端那次写入更晚（同一个时间戳分不出先后）。
    let changed = manifest(vec![entry("rel-savedata", "save.sav", 6, stamp)]);
    let merged = plan(&changed, &[target("rel-savedata", &saves)], Merge::Newer).unwrap();
    assert_eq!(merged.take.len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_manual_restore_lays_the_whole_package_back_down() {
    let dir = temp("restore");
    let saves = dir.join("saves");
    write(&saves, "save.sav", "local-and-newer");
    write(&saves, "extra.sav", "local-only");

    let cloud = manifest(vec![entry("rel-savedata", "save.sav", 1, 1)]);
    let merged = plan(&cloud, &[target("rel-savedata", &saves)], Merge::Replace).unwrap();

    // 用户点了"恢复"：以云端为准，本机更新的也盖掉。
    assert_eq!(merged.take.len(), 1);
    assert!(merged.kept.is_empty());
    // 但本机多出来的那个文件不会被删，只报给用户看。
    assert_eq!(merged.extras, vec!["rel-savedata/extra.sav".to_string()]);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_package_that_tries_to_escape_its_directory_is_refused() {
    assert!(safe_rel("saves/save.sav"));
    assert!(!safe_rel(""));
    assert!(!safe_rel("/etc/passwd"));
    assert!(!safe_rel("../escape"));
    assert!(!safe_rel("a/../../b"));
    assert!(!safe_rel("a\\b"));
    assert!(!safe_rel("a//b"));

    let dir = temp("escape");
    let saves = dir.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    let cloud = manifest(vec![entry("rel-savedata", "../escaped.sav", 1, 1)]);
    let error = plan(&cloud, &[target("rel-savedata", &saves)], Merge::Replace).unwrap_err();
    assert!(error.contains("不安全"), "{error}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_manifest_from_a_newer_kotori_is_refused_rather_than_guessed() {
    let text = format!(
        r#"{{"format":{}, "created":"2026-09-15T12:00:00Z", "entries":[]}}"#,
        FORMAT + 1
    );
    let error = parse_manifest(&text).unwrap_err();
    assert!(error.contains("更新版本"), "{error}");

    let error = parse_manifest("not json").unwrap_err();
    assert!(error.contains("JSON"), "{error}");

    // 老包没有 `locations`：位置从条目里推出来，不能因此当成"空的"。
    let legacy = r#"{"format":1,"created":"2026-09-15T12:00:00Z",
        "entries":[{"key":"rel-a","path":"x.sav","size":1,"mtime_ms":5}]}"#;
    let manifest = parse_manifest(legacy).unwrap();
    assert!(manifest.has_location("rel-a"));
    assert!(!manifest.has_location("rel-b"));
}

//! 版本快照：命名、识别与保留窗口。
//!
//! 快照目录名是整个保留策略的锚点——[`version_stamp`] 生成它，[`is_snapshot`]
//! 认人，[`prune_plan`] 只对认得的名字排删除计划；`parse_dirs` 把
//! `rclone lsf` 的输出变成目录名。与 `remote_paths` 分开：那边说的是"放在哪里"，
//! 这里说的是"时间轴上的哪一刻"。

/// Timestamp used for a version snapshot directory.
///
/// The first 16 characters are second-precision UTC, so lexicographic order
/// equals chronological order and pruning stays a simple sort. The suffix is
/// random and is what makes the name **unique**: two uploads inside the same
/// second (a game exiting while the user hits "sync now", or the safety
/// snapshot a restore takes) would otherwise share a directory, and the later
/// one would silently destroy the earlier snapshot.
pub fn version_stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    let unique: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    format!("{}-{unique}", now.format("%Y%m%dT%H%M%SZ"))
}

/// Parse `rclone lsf --dirs-only` output into sorted directory names.
pub fn parse_dirs(output: &str) -> Vec<String> {
    let mut dirs: Vec<String> = output
        .lines()
        .map(|line| line.trim().trim_end_matches('/'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    dirs.sort();
    dirs
}

/// Which version snapshots should be removed to honour the sliding window.
///
/// `keep_versions == 0` means "keep everything" and always returns empty — the
/// default, because losing an old save silently is worse than using more space.
/// Only names that look like our own snapshot stamps are ever considered.
pub fn prune_plan(versions: &[String], keep_versions: u32) -> Vec<String> {
    if keep_versions == 0 {
        return Vec::new();
    }

    let mut stamps: Vec<&String> = versions.iter().filter(|name| is_snapshot(name)).collect();
    stamps.sort();

    let keep = keep_versions as usize;
    if stamps.len() <= keep {
        return Vec::new();
    }
    stamps[..stamps.len() - keep]
        .iter()
        .map(|name| (*name).clone())
        .collect()
}

/// `20260911T101500Z` or `20260911T101500Z-1a2b3c4d` — the only names pruning
/// is allowed to touch.
///
/// Anything else in the bucket belongs to the user or to another tool, and is
/// never deleted.
pub fn is_snapshot(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 16 {
        return false;
    }
    let base = &bytes[..16];
    if !(base[8] == b'T'
        && base[15] == b'Z'
        && base[..8].iter().all(u8::is_ascii_digit)
        && base[9..15].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let rest = &name[16..];
    rest.is_empty()
        || (rest.starts_with('-')
            && rest.len() > 1
            && rest[1..].chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directory_listings() {
        let output = "20260911T101500Z/\n20260910T090000Z/\n\n";
        assert_eq!(
            parse_dirs(output),
            vec![
                "20260910T090000Z".to_string(),
                "20260911T101500Z".to_string()
            ]
        );
        assert!(parse_dirs("").is_empty());
    }

    #[test]
    fn retention_keeps_everything_by_default() {
        let versions: Vec<String> = (0..10).map(|i| format!("2026090{i}T000000Z")).collect();
        // 0 == keep everything: losing an old save is worse than using space.
        assert!(prune_plan(&versions, 0).is_empty());
        // Fewer versions than the window: nothing to do.
        assert!(prune_plan(&versions, 10).is_empty());
        assert!(prune_plan(&versions, 99).is_empty());
    }

    #[test]
    fn retention_removes_only_the_oldest_snapshots() {
        let versions = vec![
            "20260903T000000Z".to_string(),
            "20260901T000000Z".to_string(),
            "20260902T000000Z".to_string(),
        ];
        assert_eq!(
            prune_plan(&versions, 2),
            vec!["20260901T000000Z".to_string()],
            "the oldest snapshot goes first"
        );
        assert_eq!(
            prune_plan(&versions, 1),
            vec![
                "20260901T000000Z".to_string(),
                "20260902T000000Z".to_string()
            ]
        );
    }

    #[test]
    fn retention_ignores_anything_that_is_not_a_snapshot() {
        // A stray directory in the bucket must never be deleted by pruning.
        let versions = vec![
            "20260901T000000Z".to_string(),
            "20260902T000000Z".to_string(),
            "important-do-not-touch".to_string(),
            "current".to_string(),
        ];
        assert_eq!(
            prune_plan(&versions, 1),
            vec!["20260901T000000Z".to_string()]
        );
    }

    #[test]
    fn version_stamps_sort_chronologically() {
        let first = version_stamp(
            chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let second = version_stamp(
            chrono::DateTime::parse_from_rfc3339("2026-09-11T10:16:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert!(first.starts_with("20260911T101500Z"), "{first}");
        assert!(first < second, "lexicographic order must match time order");
        assert!(is_snapshot(&first), "{first}");
        assert!(is_snapshot("20260911T101500Z"));
        assert!(!is_snapshot("20260911T101500"), "no Z");
        assert!(!is_snapshot("20260911T101500Z_extra"), "only -suffix");
        assert!(!is_snapshot("20260911X101500Z"), "T separator is required");
        assert!(!is_snapshot("not-a-stamp"));
        assert!(!is_snapshot(""));
    }

    #[test]
    fn two_uploads_in_the_same_second_do_not_share_a_snapshot() {
        // Regression: snapshot directories used to be named to the second, so
        // the second upload overwrote the first one's history — and a restore's
        // safety snapshot could destroy the very snapshot being restored.
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let first = version_stamp(now);
        let second = version_stamp(now);
        assert_ne!(first, second);
        for stamp in [first, second] {
            assert!(is_snapshot(&stamp), "{stamp}");
            assert!(stamp.starts_with("20260911T101500Z-"), "{stamp}");
        }
    }
}

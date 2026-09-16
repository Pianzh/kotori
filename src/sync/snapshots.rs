//! 版本包：命名、识别与保留窗口。
//!
//! 包名是整个保留策略的锚点——[`version_stamp`] 生成它，[`is_snapshot`] 认人，
//! [`prune_plan`] 只对认得的名字排删除计划；[`parse_packages`] 把
//! `rclone lsf --files-only` 的输出变成包名列表。与 `remote_paths` 分开：那边
//! 说的是"放在哪里"，这里说的是"时间轴上的哪一刻"。

/// Timestamp used for a version package name.
///
/// `20260911T101500123Z-1a2b3c4d`: **millisecond-precision** UTC, then a random
/// suffix. Two properties, both load-bearing:
///   * lexicographic order equals chronological order, because every timestamp
///     is the same length and zero-padded — that is what makes "the newest
///     package" a simple `max` and pruning a simple sort;
///   * the name is unique, because of the random suffix.
///
/// ⚠ 从前这里只有秒精度，于是**同一秒内的两次上传谁新谁旧全看随机后缀**：
/// 游戏刚退出就点"立即同步"能撞上，e2e 里两次 `sync.now` 就撞了，结果"恢复到
/// 最新"拿到了上一版。毫秒精度把撞车压到理论上（同一毫秒还要两次上传）。
pub fn version_stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    let unique: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    format!("{}-{unique}", now.format("%Y%m%dT%H%M%S%3fZ"))
}

/// Parse `rclone lsf --files-only` output into the stamps of our own packages.
///
/// Anything that is not one of our package names is dropped here, which is what
/// makes the rest of the module safe: the list this produces is the *only* thing
/// pruning is ever allowed to consider.
pub fn parse_packages(output: &str) -> Vec<String> {
    let mut stamps: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(super::PACKAGE_SUFFIX))
        .filter(|stamp| is_snapshot(stamp))
        .map(str::to_string)
        .collect();
    stamps.sort();
    stamps
}

/// Which version packages should be removed to honour the sliding window.
///
/// `keep_versions == 0` means "keep everything" and always returns empty — the
/// default, because losing an old save silently is worse than using more space.
/// Only names that look like our own stamps are ever considered.
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

/// `20260911T101500Z`, `20260911T101500123Z`, or either of those plus
/// `-1a2b3c4d` — the only names pruning is allowed to touch.
///
/// 秒精度（老包）与毫秒精度都认，所以一个旧桶不会因为升级而"看不见自己的包"。
/// Anything else in the bucket belongs to the user or to another tool, and is
/// never deleted.
pub fn is_snapshot(name: &str) -> bool {
    let bytes = name.as_bytes();
    // 时间戳以 `Z` 结尾：19 字符是毫秒精度，16 字符是秒精度。
    let Some(stamp_len) = [19usize, 16]
        .into_iter()
        .find(|len| bytes.len() >= *len && bytes[len - 1] == b'Z')
    else {
        return false;
    };
    // `YYYYMMDDTHHMMSS[mmm]`：只有分隔符那一位不是数字。
    let digits_ok = bytes[..stamp_len - 1]
        .iter()
        .enumerate()
        .all(|(index, byte)| {
            if index == 8 {
                *byte == b'T'
            } else {
                byte.is_ascii_digit()
            }
        });
    if !digits_ok {
        return false;
    }
    let rest = &name[stamp_len..];
    rest.is_empty()
        || (rest.starts_with('-')
            && rest.len() > 1
            && rest[1..].chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_listings_and_ignores_everything_else() {
        let output = "20260911T101500Z-1a2b3c4d.zip\n20260910T090000Z.zip\n\n\
                      notes.txt\n20260909T000000Z.zip\n";
        assert_eq!(
            parse_packages(output),
            vec![
                "20260909T000000Z".to_string(),
                "20260910T090000Z".to_string(),
                "20260911T101500Z-1a2b3c4d".to_string()
            ],
            "names are sorted and stripped of .zip; foreign objects are dropped"
        );
        assert!(parse_packages("").is_empty());
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
        let at = |text: &str| {
            version_stamp(
                chrono::DateTime::parse_from_rfc3339(text)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            )
        };
        let first = at("2026-09-11T10:15:00Z");
        let second = at("2026-09-11T10:16:00Z");
        assert!(first.starts_with("20260911T101500000Z-"), "{first}");
        assert!(first < second, "lexicographic order must match time order");
        assert!(is_snapshot(&first), "{first}");

        // 毫秒精度：同一秒内也有先后，而树里的顺序就是时间的顺序。
        let within = at("2026-09-11T10:15:00.250Z");
        assert!(first < within, "{first} vs {within}");
        assert!(within < second, "{within} vs {second}");

        // 老包（秒精度）仍然认得，否则升级之后旧桶里的包会全部"消失"。
        assert!(is_snapshot("20260911T101500Z"));
        assert!(is_snapshot("20260911T101500Z-1a2b3c4d"));
        assert!(is_snapshot("20260911T101500123Z"));
        assert!(!is_snapshot("20260911T101500"), "no Z");
        assert!(!is_snapshot("20260911T101500Z_extra"), "only -suffix");
        assert!(!is_snapshot("20260911X101500Z"), "T separator is required");
        assert!(!is_snapshot("20260911T10150012Z"), "digits come in pairs");
        assert!(!is_snapshot("not-a-stamp"));
        assert!(!is_snapshot(""));
    }

    #[test]
    fn two_uploads_in_the_same_second_are_still_ordered() {
        // ⚠ 这是一条真被咬过的回归：从前时间戳只有秒精度，于是同一秒内的两次
        // 上传（游戏刚退出就点"立即同步"）谁新谁旧全看随机后缀 —— e2e 里"恢复
        // 到最新"因此拿到了上一版。
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let first = version_stamp(now);
        let second = version_stamp(now);
        assert_ne!(first, second, "两次上传不能撞名字");
        for stamp in [&first, &second] {
            assert!(is_snapshot(stamp), "{stamp}");
        }
        // 同一时刻的两个名字分不出先后（毫秒都一样），但它们与更晚的时刻之间
        // 的顺序是确定的。
        let later = version_stamp(
            chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:01Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert!(first < later && second < later, "{later}");
    }
}

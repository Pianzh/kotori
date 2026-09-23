//! 一版存档的**明细**：叫什么、多大、什么时候。
//!
//! 与 `snapshots` 分开：那边管名字（命名、识别、保留窗口 —— 删除策略全靠它），这里只管
//! "给人看的那几栏"。两个引擎在这里并成同一个形状，界面因此不必分叉：
//!
//!   * rclone：`lsjson` 一次给名字 + 大小，时间由名字里的时间戳还原；
//!   * kopia：`snapshot list --json` 里本来就有 `startTime` 与 `stats.totalSize`。
//!
//! ⚠ 大小拿不到时**如实写 0**（界面显示"不知道"），绝不编一个看着像真的数。

use serde::{Deserialize, Serialize};

/// 一版存档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    /// 版本名（就是包名去掉后缀，或 kopia 快照的 description）。
    pub name: String,
    /// 多大（字节）。引擎拿不到就是 0。
    pub size: u64,
    /// 什么时候（UTC，RFC3339，秒精度）。名字认不出来时是空串。
    pub time: String,
}

/// `rclone lsjson --files-only` 的一行（只看我们用得上的两个字段）。
#[derive(Debug, Deserialize)]
struct JsonEntry {
    #[serde(default, rename = "Name")]
    name: String,
    #[serde(default, rename = "Size")]
    size: u64,
}

/// 解析 `rclone lsjson` 的输出：**只留我们自己写的包**，最旧在前。
///
/// 判据与 `parse_packages` 那一套逐字相同（名字得像版本戳）—— 桶是用户的，里面可能
/// 塞着别的东西，那些既不该显示、更不该被当成一版存档。
pub fn parse_json_versions(output: &str) -> Result<Vec<VersionInfo>, String> {
    let entries: Vec<JsonEntry> =
        serde_json::from_str(output).map_err(|e| format!("读不懂 rclone lsjson 的输出: {e}"))?;

    let mut versions: Vec<VersionInfo> = entries
        .into_iter()
        .filter_map(|entry| {
            let name = entry.name.strip_suffix(super::PACKAGE_SUFFIX)?.to_string();
            super::is_snapshot(&name).then(|| VersionInfo {
                time: super::snapshots::stamp_time(&name)
                    .map(|moment| moment.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_default(),
                name,
                size: entry.size,
            })
        })
        .collect();
    versions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsjson_becomes_versions_with_their_sizes() {
        let listed = r#"[
            {"Path":"20260911T101500123Z-1a2b3c4d.zip","Name":"20260911T101500123Z-1a2b3c4d.zip","Size":4096,"IsDir":false},
            {"Path":"20260910T090000Z.zip","Name":"20260910T090000Z.zip","Size":128,"IsDir":false}
        ]"#;
        let versions = parse_json_versions(listed).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].name, "20260910T090000Z", "最旧在前");
        assert_eq!(versions[0].size, 128);
        assert_eq!(versions[1].size, 4096);
        // 时间由名字还原（UTC），界面上再换成本机时区。
        assert_eq!(versions[0].time, "2026-09-10T09:00:00Z");
        assert_eq!(versions[1].time, "2026-09-11T10:15:00Z");
    }

    #[test]
    fn strangers_in_the_bucket_are_never_shown_as_versions() {
        let listed = r#"[
            {"Name":"kotori-game.json","Size":300},
            {"Name":"notes.txt","Size":10},
            {"Name":"20260911T101500Z.zip","Size":7},
            {"Name":"20260911T101500Z_extra.zip","Size":7}
        ]"#;
        let versions = parse_json_versions(listed).unwrap();
        assert_eq!(versions.len(), 1, "{versions:?}");
        assert_eq!(versions[0].name, "20260911T101500Z");

        // 空目录是空列表，不是错误；坏 JSON 才是错误（"没有"与"读坏了"分得开）。
        assert!(parse_json_versions("[]").unwrap().is_empty());
        assert!(parse_json_versions("not json").is_err());
    }
}

//! kopia 那边的**输出解析**：把 `snapshot list --json` 变成我们认的东西。
//!
//! 从 `kopia_args.rs` 拆出来：那边是"参数长什么样"，这里是"跑完以后怎么读结果"。
//! 两者共用的只有"我们自己的快照长什么样"这一层，拆开之后两边都读得完。
//!
//! 解析失败一律如实报错，绝不猜 —— 猜错就是把别人的东西当成我们的一版存档。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::sync::cloud::{CloudGame, KIND_IDENTITY};
use crate::sync::is_snapshot;

/// `snapshot list --json` 里的一条。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Snapshot {
    pub id: String,
    /// 我们自己写进去的版本名；不是 kotori 建的快照这里是空的。
    #[serde(default)]
    pub description: String,
    /// 快照上的标签。
    ///
    /// ⚠ 实测（kopia 0.22.3）：JSON 里的键带 `tag:` 前缀 —— `--tags=game:3days`
    /// 打出来的是 `{"tag:game":"3days"}`。读标签请走 [`Snapshot::tag`]。
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// 快照开始时刻（kopia 的 RFC3339，纳秒精度）。
    ///
    /// 身份快照的描述是固定的一句话，所以"最新的那条"只能靠时间认（见
    /// [`identity_snapshots`]）。定长零填充的 RFC3339 字符串可以直接比大小。
    #[serde(default, rename = "startTime")]
    pub start_time: String,
    /// 这条快照占了多大（kopia 自己算的）。界面上的"这一版多大"就是它。
    #[serde(default)]
    pub stats: SnapshotStats,
}

/// `snapshot list --json` 里每个快照的 `stats`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SnapshotStats {
    #[serde(default, rename = "totalSize")]
    pub total_size: u64,
}

impl Snapshot {
    /// 读一个标签，带不带 `tag:` 前缀都认（前缀是 kopia 的输出细节，不是我们的语义）。
    pub(super) fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .get(key)
            .or_else(|| self.tags.get(&format!("tag:{key}")))
            .map(String::as_str)
    }

    /// 这一条是不是我们自己拍的。
    ///
    /// 两条判据任一成立即可：描述长成我们自己的版本名（存档快照），或者带着
    /// `kind` 标签（身份快照的描述是一句固定的话，本来就不该长得像版本名）。
    fn is_ours(&self) -> bool {
        is_snapshot(&self.description) || self.tag("kind").is_some()
    }

    /// 这一条是身份快照吗（不是"一版存档"）。
    fn is_identity(&self) -> bool {
        self.tag("kind") == Some(KIND_IDENTITY)
    }
}

/// 按 `game:` 标签把仓库里的快照归到各自的游戏名下。
///
/// 这就是"云端有哪几款游戏"：kopia 的仓库是一个不透明的大块，能认人的只有标签。
/// 没带 `game:` 标签、也不是我们拍的快照一律不看 —— 用户可能拿同一个仓库放着别的
/// 备份，那些东西不该出现在游戏的列表里。
pub(super) fn parse_cloud_games(text: &str) -> Result<Vec<CloudGame>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut games: BTreeMap<String, CloudGame> = BTreeMap::new();
    for snapshot in all {
        if !snapshot.is_ours() {
            continue;
        }
        let Some(id) = snapshot.tag("game") else {
            continue;
        };
        let game = games.entry(id.to_string()).or_insert_with(|| CloudGame {
            id: id.to_string(),
            versions: 0,
            latest: None,
            size: 0,
        });
        // 身份快照不是"一版存档"：它只有一句话，没有存档内容。
        if is_snapshot(&snapshot.description) {
            game.versions += 1;
            // 快照不可变、描述就是版本名，所以"最近一版"直接比名字（与 rclone 同一条判据），
            // 大小在同一份 JSON 里（`stats.totalSize`）—— 一次调用就把摘要拿全了。
            let newer = game
                .latest
                .as_deref()
                .is_none_or(|known| known < snapshot.description.as_str());
            if newer {
                game.latest = Some(snapshot.description.clone());
                game.size = snapshot.stats.total_size;
            }
        }
    }
    Ok(games.into_values().collect())
}

/// 每个云端身份最新的那条**身份快照**：`(cloud_id, 快照 id)`。
///
/// 同一个身份每次上传都会再拍一条（快照不可变，改不了旧的），所以这里按时间取最新
/// 的一条。读它的内容还要一次 `kopia restore`（§5.4：一次读 = 起一个进程），
/// 所以调用方要缓存。
pub(super) fn identity_snapshots(text: &str) -> Result<Vec<(String, String)>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut newest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snapshot in all {
        if !snapshot.is_identity() {
            continue;
        }
        let Some(cloud_id) = snapshot.tag("game") else {
            continue;
        };
        match newest.get(cloud_id) {
            Some(known) if known.start_time >= snapshot.start_time => {}
            _ => {
                newest.insert(cloud_id.to_string(), snapshot);
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(cloud_id, snapshot)| (cloud_id, snapshot.id))
        .collect())
}

/// 某个身份（`game:<cloud_id>`）在云端**所有的**身份快照 id。
///
/// ⚠ 与 [`identity_snapshots`] 的差别只有一条，但很关键：那个每个身份只留**最新**一张
/// （"读卡"要最新那张就够了），而**删词条**必须把旧卡一起删掉 —— 每上传一次就会补拍一张
/// 身份快照，只删最新那张的话，旧卡照样带着 `game:` 标签躺在仓库里，而 `cloud_games` 正是
/// 从标签认人的，于是"删掉词条"之后这一款在云端仍然列得出来（2026-09-26 CI 真机上红的
/// 就是这个：删完还剩 1 款）。
pub(super) fn identity_snapshot_ids(text: &str, cloud_id: &str) -> Result<Vec<String>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    Ok(all
        .into_iter()
        .filter(|snapshot| snapshot.is_identity() && snapshot.tag("game") == Some(cloud_id))
        .map(|snapshot| snapshot.id)
        .collect())
}

/// 解析 `snapshot list --json`，只留下**我们自己建的**那些，按版本名排序。
///
/// 判据与 rclone 那条路同一套（[`super::super::is_snapshot`]）：描述不像我们写的
/// 版本名就绝不碰——用户可能拿同一个 kopia 仓库放着别的东西。
pub(super) fn parse_snapshots(text: &str) -> Result<Vec<Snapshot>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut ours: Vec<Snapshot> = all
        .into_iter()
        .filter(|snapshot| {
            // 描述得像版本名，而且**不是**身份快照：两种快照躺在同一个仓库里，
            // 版本列表与保留窗口只许看存档那一种（§5.3）。
            super::super::is_snapshot(&snapshot.description) && !snapshot.is_identity()
        })
        .collect();
    ours.sort_by(|a, b| a.description.cmp(&b.description));
    Ok(ours)
}

/// 一版存档的明细（名字 + 大小 + 时间）：`snapshot list --json` 里全都有。
///
/// 与 [`parse_snapshots`] 是同一条判据（只认我们自己的存档快照），只是这里连大小和时间
/// 一起带出来 —— 界面上"点开看每一版"要的就是这三栏。
pub(super) fn parse_version_infos(text: &str) -> Result<Vec<crate::sync::VersionInfo>, String> {
    let mut versions: Vec<crate::sync::VersionInfo> = parse_snapshots(text)?
        .into_iter()
        .map(|snapshot| crate::sync::VersionInfo {
            name: snapshot.description.clone(),
            size: snapshot.stats.total_size,
            time: snapshot.start_time.clone(),
        })
        .collect();
    versions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(versions)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_snapshots_we_named_ourselves_are_ours() {
        let json = r#"[
          {"id":"aaa","description":"20260916T120000000Z-abcd1234","startTime":"2026-09-16T12:00:00Z"},
          {"id":"bbb","description":"","startTime":"2026-09-16T13:00:00Z"},
          {"id":"ccc","description":"my own backup","startTime":"2026-09-16T14:00:00Z"},
          {"id":"ddd","description":"20260915T100000Z","startTime":"2026-09-15T10:00:00Z"}
        ]"#;
        let ours = parse_snapshots(json).unwrap();
        // 空的、以及别人随手写的描述都不算；老格式（16 位）仍然认。
        assert_eq!(
            ours.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["ddd", "aaa"],
            "按版本名排序，最旧在前"
        );
    }

    #[test]
    fn version_infos_carry_the_size_and_the_moment_of_each_version() {
        let json = r#"[
          {"id":"a","description":"20260916T120000000Z-abcd1234","startTime":"2026-09-16T12:00:00Z","stats":{"totalSize":4096}},
          {"id":"b","description":"20260915T100000Z","startTime":"2026-09-15T10:00:00Z","stats":{"totalSize":128}},
          {"id":"c","description":"kotori-identity","startTime":"2026-09-16T13:00:00Z","tags":{"tag:kind":"identity"}},
          {"id":"d","description":"my own backup","startTime":"2026-09-16T14:00:00Z"}
        ]"#;
        let versions = parse_version_infos(json).unwrap();
        assert_eq!(
            versions.len(),
            2,
            "身份快照与别人的东西都不是一版: {versions:?}"
        );
        assert_eq!(versions[0].name, "20260915T100000Z", "最旧在前");
        assert_eq!(versions[0].size, 128);
        assert_eq!(versions[0].time, "2026-09-15T10:00:00Z");
        assert_eq!(versions[1].size, 4096);
        // 少了 `stats` 就如实当 0（"不知道"），绝不编一个看着像真的数。
        let without = r#"[{"id":"a","description":"20260916T120000000Z","startTime":"x"}]"#;
        assert_eq!(parse_version_infos(without).unwrap()[0].size, 0);
    }

    #[test]
    fn cloud_games_come_from_the_game_tag_and_identity_snapshots_are_not_versions() {
        // 实测（0.22.3）：JSON 里的标签键带 `tag:` 前缀。
        let json = r#"[
          {"id":"a","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"save"},"stats":{"totalSize":4096}},
          {"id":"b","description":"20260916T130000000Z-abcd1234","tags":{"tag:game":"3days"},"stats":{"totalSize":512}},
          {"id":"c","description":"20260915T100000Z","tags":{"game":"life-game"},"stats":{"totalSize":128}},
          {"id":"d","description":"kotori-identity","tags":{"tag:game":"life-game","tag:kind":"identity"}},
          {"id":"e","description":"someone else's backup","tags":{"tag:game":"not-ours"}},
          {"id":"f","description":"20260916T140000000Z-abcd1234","tags":{}},
          {"id":"g","description":"my own backup","tags":{}}
        ]"#;
        let games = parse_cloud_games(json).unwrap();
        assert_eq!(
            games,
            vec![
                // 两个存档快照（一个带 kind、一个不带都算），身份快照不算版本；
                // `latest`/`size` 是"名字最大的那一版"以及它自己的 `stats.totalSize`。
                CloudGame {
                    id: "3days".to_string(),
                    versions: 2,
                    latest: Some("20260916T130000000Z-abcd1234".to_string()),
                    size: 512
                },
                CloudGame {
                    id: "life-game".to_string(),
                    versions: 1,
                    latest: Some("20260915T100000Z".to_string()),
                    size: 128
                },
            ],
            "按 id 排序；没有 game 标签的、以及别人拍的一律不出现"
        );
        assert!(parse_cloud_games("[]").unwrap().is_empty());
        assert!(parse_cloud_games("not json").is_err());
    }

    #[test]
    fn broken_json_is_reported_not_swallowed() {
        assert!(parse_snapshots("not json").is_err());
    }
    #[test]
    fn identity_snapshots_are_never_counted_as_versions() {
        // 即使描述被改成了版本名，`kind=identity` 也说了算：保留窗口绝不许碰它。
        let json = r#"[
              {"id":"s1","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"save"},"startTime":"2026-09-16T12:00:00Z"},
              {"id":"i1","description":"20260915T000000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"identity"},"startTime":"2026-09-15T10:00:00Z"}
            ]"#;
        let ours = parse_snapshots(json).unwrap();
        assert_eq!(
            ours.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["s1"],
            "身份快照不是一版存档"
        );

        // 身份快照按**时间**取最新的一条（描述是固定的一句话，分不出新旧）。
        let json = r#"[
              {"id":"old","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-15T10:00:00Z"},
              {"id":"new","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-16T10:00:00Z"},
              {"id":"c2","description":"kotori-identity","tags":{"tag:game":"c2","tag:kind":"identity"},"startTime":"2026-09-14T10:00:00Z"},
              {"id":"save","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"c1","tag:kind":"save"},"startTime":"2026-09-17T10:00:00Z"}
            ]"#;
        assert_eq!(
            identity_snapshots(json).unwrap(),
            vec![
                ("c1".to_string(), "new".to_string()),
                ("c2".to_string(), "c2".to_string()),
            ],
            "每个身份取最新那条；存档快照不算身份快照"
        );
    }

    /// 删词条要的是**整族**身份快照：只删最新那张，旧卡还带着 `game:` 标签躺着，这一款在
    /// 云端就仍然列得出来（2026-09-26 真机 CI 上的红就是这个）。
    #[test]
    fn every_identity_snapshot_of_one_game_is_found_not_just_the_newest() {
        let json = r#"[
              {"id":"old","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-15T10:00:00Z"},
              {"id":"new","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-16T10:00:00Z"},
              {"id":"other","description":"kotori-identity","tags":{"tag:game":"c2","tag:kind":"identity"},"startTime":"2026-09-14T10:00:00Z"},
              {"id":"save","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"c1","tag:kind":"save"},"startTime":"2026-09-17T10:00:00Z"}
            ]"#;
        assert_eq!(
            identity_snapshot_ids(json, "c1").unwrap(),
            vec!["old".to_string(), "new".to_string()],
            "同一个身份的所有旧卡都要删；别人的卡与存档快照不许碰"
        );
        assert_eq!(
            identity_snapshot_ids(json, "c9").unwrap(),
            Vec::<String>::new(),
            "没有这一款就一个都不删"
        );
    }
}

//! kopia 那边的**索引**参数与输出解析：全是纯函数，不碰进程、不碰网络。
//!
//! 从 `kopia_args.rs` 拆出来：那个文件是"一版存档怎么拍、怎么列"，而索引是另一类对象
//! （一个桶一份，见 `crate::sync::index`）—— 两者只有 `snapshot list --json` 这一层
//! 是共用的。拆开之后两边都读得完。

use std::collections::BTreeMap;

use super::kopia_args::Snapshot;
use crate::sync::cloud::KIND_INDEX;

/// 索引快照的 description：**一句固定的话**，绝不能长得像版本名（否则会被
/// `kopia_args::parse_snapshots` 当成一版存档）。
pub(super) const INDEX_DESCRIPTION: &str = "kotori-index";

/// 拍一条**索引快照**：源目录里只有那份 `kotori-index.json`。
///
/// `slot` 是 `index:` 标签的值：合并快照是 [`crate::sync::index::INDEX_MAIN`]，增量是它自己的对象名。
/// 一条快照一个槽位，所以"同一个槽位的最新那条"就是当前值（快照不可变，改不了旧的）。
pub(super) fn index_snapshot_args(slot: &str, source: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "create".to_string(),
        "--json".to_string(),
        format!("--tags=kind:{KIND_INDEX}"),
        format!("--tags=index:{slot}"),
        format!("--description={INDEX_DESCRIPTION}"),
        source.to_string(),
    ]
}

/// 每个槽位**最新**的那条索引快照：`(slot, 快照 id)`。
///
/// 与 [`identity_snapshots`] 同一套做法：快照不可变，所以"当前值"= 时间最新的那一条。
pub(super) fn index_snapshots(text: &str) -> Result<Vec<(String, String)>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut newest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snapshot in all {
        if snapshot.tag("kind") != Some(KIND_INDEX) {
            continue;
        }
        let Some(slot) = snapshot.tag("index") else {
            continue;
        };
        match newest.get(slot) {
            Some(known) if known.start_time >= snapshot.start_time => {}
            _ => {
                newest.insert(slot.to_string(), snapshot);
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(slot, snapshot)| (slot, snapshot.id))
        .collect())
}

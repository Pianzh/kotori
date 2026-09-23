//! 「云端存档」那一页的回包：清单、某一款的版本明细，以及**添加页**问的那一次匹配。
//!
//! 从 `parse/sync.rs` 拆出来：那边是"本机的同步设置、凭据、配对"，这里是"云端有什么"。
//!
//! `sync.cloud_list` 与 `sync.match` 回的是**同一形状的一行**（都来自索引里那一条），
//! 所以行解析只有一份（[`parse_cloud_row`]）—— 两处口径分家的话，"列表里说本机认了、
//! 添加页说本机没有"这种事迟早发生。

use crate::ui::*;

/// `sync.cloud_list` 的回包：**一个桶一份的索引**读出来的云端清单。
///
/// `indexed` 与"云端没有游戏"是两件事：前者 = 桶里还没建过索引（要去点一次深度扫描），
/// 后者 = 建过了、但云端确实没东西。界面上那两句话完全不同。
pub(in crate::ui) fn parse_cloud_list(value: &Value) -> Result<(bool, Vec<CloudGameRow>), String> {
    let (indexed, games) = indexed_and_games(value)?;
    Ok((indexed, games.iter().map(parse_cloud_row).collect()))
}

/// `sync.match` 的回包：这个 exe 在云端是哪一款（0 条 = 云端没有它）。
///
/// 与 [`parse_cloud_list`] 同形，差别只在**条数**：列表给的是云端全部，这里给的是
/// 指纹对得上的那几条。`indexed == false` 时 `games` 恒空 —— 那表示桶里还没建过索引，
/// 与"云端没有这一款"是两句话。
pub(in crate::ui) fn parse_cloud_match(value: &Value) -> Result<(bool, Vec<CloudGameRow>), String> {
    let (indexed, games) = indexed_and_games(value)?;
    Ok((indexed, games.iter().map(parse_cloud_row).collect()))
}

/// 两个回包共用的开头：`indexed` 缺了就是坏回包（"还没建索引"与"云端没有"绝不能混）。
fn indexed_and_games(value: &Value) -> Result<(bool, &Vec<Value>), String> {
    let indexed = value
        .get("indexed")
        .and_then(Value::as_bool)
        .ok_or_else(|| "回包里没有 indexed".to_string())?;
    let games = value
        .get("games")
        .and_then(Value::as_array)
        .ok_or_else(|| "回包里没有 games".to_string())?;
    Ok((indexed, games))
}

/// 索引里的一行 → 界面那一行（`sync.cloud_list` 与 `sync.match` 共用）。
fn parse_cloud_row(game: &Value) -> CloudGameRow {
    let text = |key: &str| {
        game.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    CloudGameRow {
        cloud_key: text("cloud_key"),
        cloud_id: text("cloud_id"),
        name: text("name"),
        machines: game.get("machines").and_then(Value::as_u64).unwrap_or(0) as usize,
        versions: game.get("versions").and_then(Value::as_u64).unwrap_or(0) as usize,
        latest: game
            .get("latest")
            .and_then(Value::as_str)
            .map(str::to_string),
        size: game.get("size").and_then(Value::as_u64).unwrap_or(0),
        exe_paths: game
            .get("exe_paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        local_id: text("local_id"),
        local_name: text("local_name"),
        rejected: game
            .get("rejected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// `sync.cloud_versions` 的回包：某一款在云端有哪几版（最旧在前）。
///
/// `versions` 缺失或不是数组都算坏回包 —— 空的版本列表与"没问成"必须分得开，
/// 不然界面会把一次失败画成"云端一版都没有"。
pub(in crate::ui) fn parse_cloud_versions(value: &Value) -> Result<Vec<CloudVersionRow>, String> {
    let versions = value
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| "回包里没有 versions".to_string())?;
    // 每一版是一个对象（名字 + 大小 + 时间）。只有名字的老形状也收着认 —— 界面上少显示
    // 一栏，总比整块报错强。
    Ok(versions
        .iter()
        .map(|version| match version {
            Value::String(name) => CloudVersionRow {
                name: name.clone(),
                ..CloudVersionRow::default()
            },
            other => CloudVersionRow {
                name: other
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                size: other.get("size").and_then(Value::as_u64).unwrap_or(0),
                time: other
                    .get("time")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cloud_list_keeps_the_cloud_name_and_the_local_side_apart() {
        let payload = serde_json::json!({
            "indexed": true,
            "games": [
                {
                    "cloud_key": "original-name", "cloud_id": "c1", "name": "云端记下的名字",
                    "machines": 2, "versions": 3, "latest": "20260911T101500Z", "size": 4096,
                    "exe_paths": ["/games/original/game.exe"],
                    "local_id": "renamed", "local_name": "本机这一款", "rejected": false,
                },
                {
                    "cloud_key": "only-there", "cloud_id": "c2", "name": "本机没有的那一款",
                    "machines": 1, "versions": 0, "latest": null, "size": 0, "exe_paths": [],
                    "local_id": "", "local_name": "", "rejected": true,
                },
            ],
        });
        let (indexed, rows) = parse_cloud_list(&payload).unwrap();
        assert!(indexed);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "云端记下的名字", "显示的是名字，不是落点");
        assert_eq!(rows[0].local_label(), "本机《本机这一款》");
        assert_eq!(rows[1].local_label(), "你说过不是这一款");
        assert_eq!(rows[1].versions_label(), "还没有存档");

        // `indexed` 缺了就是坏回包 —— "还没建索引"与"云端没有游戏"绝不能混。
        assert!(parse_cloud_list(&serde_json::json!({ "games": [] })).is_err());
        assert!(parse_cloud_list(&serde_json::json!({ "indexed": false })).is_err());
    }

    #[test]
    fn versions_come_back_with_their_size_and_time() {
        let payload = serde_json::json!({
            "versions": [
                { "name": "20260910T090000Z", "size": 128, "time": "2026-09-10T09:00:00Z" },
                { "name": "20260911T101500123Z-1a2b3c4d", "size": 4096, "time": "2026-09-11T10:15:00Z" },
            ],
        });
        let versions = parse_cloud_versions(&payload).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].size, 128);
        assert_eq!(versions[1].size_label(), "4.0 KiB");
        // 只有名字的老形状也认（少一栏，不是报错）。
        let bare = serde_json::json!({ "versions": ["20260910T090000Z"] });
        assert_eq!(parse_cloud_versions(&bare).unwrap().len(), 1);
        // 空的版本列表与"没问成"分得开。
        assert!(
            parse_cloud_versions(&serde_json::json!({ "versions": [] }))
                .unwrap()
                .is_empty()
        );
        assert!(parse_cloud_versions(&serde_json::json!({ "ok": true })).is_err());
    }

    /// 匹配那一问与清单**同一形状**：落点必须拿得到（认领时要拿它当 `cloud_key`），
    /// 而"还没建索引"与"云端没有这一款"是两句话。
    #[test]
    fn a_match_reply_carries_the_key_the_binding_needs() {
        let payload = serde_json::json!({
            "indexed": true,
            "fingerprint": "v1:4110:abcd",
            "games": [{
                "cloud_key": "sg", "cloud_id": "c1", "name": "那一款", "machines": 1,
                "versions": 2, "latest": "20260911T101500Z", "size": 1024,
                "exe_paths": [], "local_id": "", "local_name": "", "rejected": false,
            }],
        });
        let (indexed, rows) = parse_cloud_match(&payload).unwrap();
        assert!(indexed);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cloud_key, "sg", "认领要用它当落点");
        assert_eq!(rows[0].cloud_id, "c1");
        // 最近一版那行是"本机时区的时间 + 大小"（时区随机器变，所以只钉大小）。
        assert!(
            rows[0].latest_label().contains("1.0 KiB"),
            "{}",
            rows[0].latest_label()
        );

        let (indexed, rows) =
            parse_cloud_match(&serde_json::json!({ "indexed": false, "games": [] })).unwrap();
        assert!(!indexed, "桶里还没索引");
        assert!(rows.is_empty());
        assert!(parse_cloud_match(&serde_json::json!({ "games": [] })).is_err());
    }
}

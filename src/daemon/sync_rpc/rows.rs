//! 云端索引里那一条怎么变成界面要的那一行 —— 「云端存档」页与**添加页的匹配**共用它。
//!
//! 两处必须说同一句话（哪个身份被本机哪一条认了、用户否过谁）。各写一份的话，"列表里
//! 说本机认了、匹配里说本机没有"这种事迟早发生，而配对唯一的错误是不可逆的。
//!
//! 这里全是纯映射 + 一次本机查表，**一个网络都不打**：云端那一侧早由索引读回来了。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::config::Config;
use crate::sync::index::IndexGame;

/// 本机这一侧的两笔账：`cloud_id -> (本机 id, 本机名字)`，以及用户明确否过的身份。
pub(super) type Locals = (HashMap<String, (String, String)>, Vec<String>);

/// 从配置里查这两笔账（本地查表，不碰网络）。
pub(super) fn locals(config: &Config) -> Locals {
    let mut paired: HashMap<String, (String, String)> = HashMap::new();
    let mut rejected: Vec<String> = Vec::new();
    for (id, game) in &config.games {
        if let Some(cloud_id) = &game.cloud_id {
            paired.insert(cloud_id.clone(), (id.clone(), game.name.clone()));
        }
        rejected.extend(game.cloud_rejected.iter().cloned());
    }
    (paired, rejected)
}

/// 索引里的这一条 → 界面要的那一行。
///
/// `local` 是本机认了它的是哪一条（没有就是 `None`），`rejected` 是"用户说过不是它"。
/// 两者都由一次 [`locals`] 得来 —— 调用点别自己再查一遍，不然两处口径会分家。
pub(super) fn game_json(
    game: &IndexGame,
    local: Option<&(String, String)>,
    rejected: bool,
) -> Value {
    json!({
        "cloud_key": game.cloud_key,
        "cloud_id": game.identity.cloud_id,
        "name": game.identity.name,
        "machines": game.identity.machines.len(),
        "versions": game.versions,
        "latest": game.latest,
        "size": game.size,
        // 用过的 exe 路径：只给人看、只给搜索用（不参与任何判断）。
        "exe_paths": game
            .identity
            .machines
            .iter()
            .flat_map(|machine| machine.exe_paths.clone())
            .collect::<Vec<String>>(),
        "local_id": local.map(|(id, _)| id.clone()).unwrap_or_default(),
        "local_name": local.map(|(_, name)| name.clone()).unwrap_or_default(),
        "rejected": rejected,
    })
}

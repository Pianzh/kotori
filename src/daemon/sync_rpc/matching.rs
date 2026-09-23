//! `sync.match`：添加游戏时那一问 —— "这个 exe 在云端是哪一款"。
//!
//! 用户 2026-09-23 要的是「填完 exe 就在本页把云端那一款认出来，认出来就直接在本页
//! 确定」。认领本身走的是**已有的** `sync.pair`（那里是唯一写身份/落点的入口），这里
//! 只回答"有没有、是哪一条"，**一个字都不写**。
//!
//! 为什么按**指纹**认而不是按名字：两台机器给同一款游戏起的名字可以不一样，指纹是唯一
//! 能认出"这一款就是这一款"的判据（见 `crate::sync::fingerprint`）。
//!
//! ⚠ 读的是**本机缓存**（见 [`Daemon::cloud_index_view`]）：填完 exe 就该当场有答案，不该
//! 等一趟网络。只有缓存过了一小时、或者本地还没有缓存时才会真的去云端 —— 那时它自己会
//! 顺手把缓存刷上。
//!
//! ⚠ **绝不卡住添加**：读不到（没配云同步、网络不通）一律如实回话，界面照旧能把本机这一条
//! 建起来，等第一次上传时再认领。

use std::path::Path;

use serde_json::{Value, json};

use super::Daemon;
use super::rows;

impl Daemon {
    /// 0 条 = 云端没有它，1 条 = 就是它，≥2 条 = 列出来**问**（与配对同一条规矩）。
    ///
    /// 指纹**由 daemon 现算**：不让客户端递进来，因为它是自动绑定的唯一依据 —— 递进来
    /// 就等于"这就是同一款"随手可写（与 `exe_fingerprint` 不进 `GamePatch` 同一条规矩）。
    pub(in crate::daemon) async fn rpc_sync_match(&self, exe: &Path) -> Result<Value, String> {
        let fingerprint = crate::sync::fingerprint::of_file(exe).ok_or_else(|| {
            format!(
                "读不出这个文件的指纹（文件在不在、有没有权限？）: {}",
                exe.display()
            )
        })?;

        // 只读本机缓存（过期/没有时才联网，理由是那条路自己会解释）。
        let view = self.cloud_index_view(false).await?;

        let (paired, rejected) = {
            let config = self.config.read().await;
            rows::locals(&config)
        };
        let indexed = view.index.is_some();
        let games: Vec<Value> = view
            .index
            .unwrap_or_default()
            .by_fingerprint(&fingerprint)
            .into_iter()
            .map(|game| {
                let cloud_id = game.identity.cloud_id.clone();
                rows::game_json(game, paired.get(&cloud_id), rejected.contains(&cloud_id))
            })
            .collect();

        Ok(json!({
            "indexed": indexed,
            "from_cache": view.from_cache,
            "cached_at": view.cached_at,
            "fingerprint": fingerprint,
            "games": games,
        }))
    }
}

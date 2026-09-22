//! 云同步的「身份」：本机这一款 = 云端哪一条档案。
//!
//! 这一层只做两件事：**认领**（第一次上传时给这一款定一个 `cloud_id`，之后粘住）
//! 与**报出本机的机器身份**（`machine_id`）。
//!
//! 为什么身份不能是游戏名、也不能是配置里的那个 id（`games.<id>`）：两台机器可能
//! 给同一款游戏起不同的名字（于是永远对不上），也可能**同一个名字是两款不同的
//! 游戏**（slug 归一化，于是把别人的存档铺进来 —— 静默损坏存档那条路）。
//!
//! 粘住是刻意的：身份一旦会自己变，跨机器就再也说不清"我刚才传的那一版是谁的"。
//! 指纹只当**提议**，改身份要用户点头（配对界面）。
//!
//! 与 `mod.rs` 分开：那边是"设置与状态"，这边只碰身份本身。

use serde_json::{Value, json};
use uuid::Uuid;

use super::Daemon;
use crate::sync::SaveTarget;
use crate::sync::cloud::{PackIdentity, short_id};

impl Daemon {
    /// 这一款在云端的身份；还没认领过就是 `None`。
    pub(super) async fn cloud_id_of(&self, game_id: &str) -> Result<Option<String>, String> {
        let config = self.config.read().await;
        let game = config
            .games
            .get(game_id)
            .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
        Ok(game.cloud_id.clone())
    }

    /// 上传这一版要写进包里的身份（缺的字段当场补上并落盘）。
    pub(super) async fn pack_identity(
        &self,
        game_id: &str,
        targets: &[SaveTarget],
    ) -> Result<PackIdentity, String> {
        let cloud_id = match self.cloud_id_of(game_id).await? {
            Some(cloud_id) => cloud_id,
            None => self.claim_cloud_id(game_id).await?,
        };
        Ok(PackIdentity {
            cloud_id,
            machine_id: Some(self.machine_id().await?),
            // 指纹是下一步的事：这里如实留空（`None` = "还不知道"），绝不编一个。
            fingerprint: None,
            // 这台机器上这一款**配了**哪些位置 —— 位置对齐（§2.7）要的就是这份清单，
            // 而不是"这一次恰好有文件的那几个"。
            locations: targets.iter().map(|target| target.key.clone()).collect(),
        })
    }

    /// 认领一个云端身份：只在"还没有"时用，写下去就粘住。
    async fn claim_cloud_id(&self, game_id: &str) -> Result<String, String> {
        let minted = Uuid::new_v4().to_string();
        let owner = game_id.to_string();
        let assigned = minted.clone();
        self.mutate_config(move |config| {
            let game = config
                .games
                .get_mut(&owner)
                .ok_or_else(|| format!("配置中找不到游戏: {owner}"))?;
            game.cloud_id = Some(assigned);
            Ok(Value::Null)
        })
        .await?;
        tracing::info!("{game_id}: 认领云端身份 {}", short_id(&minted));
        Ok(minted)
    }

    /// 本机的机器身份：第一次要它时生成并落盘。
    ///
    /// 用 `get_or_insert` 而不是"先读再写"，是因为两个上传可能同时走到这里 ——
    /// 谁都行，但**一台机器只能有一个**（身份卡上要拿它对账）。
    pub(super) async fn machine_id(&self) -> Result<String, String> {
        if let Some(id) = self.config.read().await.daemon.machine_id.clone() {
            return Ok(id);
        }
        let minted = Uuid::new_v4().to_string();
        let candidate = minted.clone();
        let value = self
            .mutate_config(move |config| {
                let id = config.daemon.machine_id.get_or_insert(candidate);
                Ok(json!(id.clone()))
            })
            .await?;
        Ok(value.as_str().map(str::to_string).unwrap_or(minted))
    }
}

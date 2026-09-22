//! 配对的三个 RPC：扫一遍、绑一条、否掉一条。
//!
//! 逻辑本身在 `sync::pairing`（纯函数，可单测）；这里只做三件事：把配置读成"本机有
//! 哪些档案"、把云端读成"有哪些身份"、把结论落盘。
//!
//! ⚠ 「扫描」是这一族里唯一会碰网络的（kopia 那边**读一次身份 = 一次 restore**，
//! §5.4），所以它就是那个"刷新云端清单"的按钮：不挂在状态刷新上，只在用户按下时跑。

use serde_json::{Value, json};

use super::Daemon;
use crate::sync::pairing::{CloudCard, LocalGame};

impl Daemon {
    /// 扫一遍云端与本机，给出配对表。
    ///
    /// **指纹唯一命中**的那些在这一次扫描里直接绑上（界面上写明"已自动绑定"并给一个
    /// 「不是同一款」）；其余的一律只列出来问。
    pub(in crate::daemon) async fn rpc_sync_pairing(&self) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;

        // 指纹是配对的判据，缺了指纹的档案在这一趟里根本认不出云端那一款。这里按需
        // 补齐（三个时刻里的最后一个：添加时、exe 换时、扫描前）。
        let filled = self.fill_fingerprints().await?;
        if filled > 0 {
            tracing::info!("配对扫描前补了 {filled} 条 exe 指纹");
        }

        let clouds: Vec<CloudCard> = runner
            .read_identities()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(key, identity)| CloudCard { key, identity })
            .collect();
        let locals = self.local_games().await;
        let plan = crate::sync::pairing::plan(&locals, &clouds);

        for (local_id, key, cloud_id) in &plan.bindings {
            self.remember_identity(local_id, cloud_id, key).await?;
        }
        tracing::info!(
            "配对扫描: 云端 {} 条身份, 自动绑定 {} 条",
            plan.rows.len(),
            plan.bindings.len()
        );

        // 回包的形状是**界面要的那一份**（名字、状态码、候选），不是内部结构：
        // 界面不该自己去查表把 id 翻成名字。
        let rows: Vec<Value> = plan.rows.iter().map(|row| row_json(row, &locals)).collect();
        Ok(json!({
            "rows": rows,
            "bound": plan.bindings.len(),
        }))
    }

    /// 用户点了一条：把本机这一款绑到这个云端身份上。
    pub(in crate::daemon) async fn rpc_sync_pair(
        &self,
        local_id: &str,
        cloud_key: &str,
        cloud_id: &str,
    ) -> Result<Value, String> {
        let owner = local_id.to_string();
        let key = cloud_key.to_string();
        let cloud = cloud_id.to_string();
        self.mutate_config(move |config| {
            let game = config
                .games
                .get_mut(&owner)
                .ok_or_else(|| format!("配置中找不到游戏: {owner}"))?;
            game.cloud_id = Some(cloud.clone());
            game.cloud_dir = Some(key);
            // 绑上就等于用户改主意了：把"不是同一款"那笔账抹掉。
            game.cloud_rejected.retain(|rejected| rejected != &cloud);
            Ok(json!({ "ok": true }))
        })
        .await
    }

    /// 用户按了「不是同一款」：撤掉绑定，并**记住**别再自动绑它。
    ///
    /// 这份记忆是自动绑定唯一的刹车：没有它，下一次扫描会把用户刚否掉的绑定又绑回来。
    pub(in crate::daemon) async fn rpc_sync_reject(
        &self,
        local_id: &str,
        cloud_id: &str,
    ) -> Result<Value, String> {
        let owner = local_id.to_string();
        let cloud = cloud_id.to_string();
        self.mutate_config(move |config| {
            let game = config
                .games
                .get_mut(&owner)
                .ok_or_else(|| format!("配置中找不到游戏: {owner}"))?;
            if !game.cloud_rejected.contains(&cloud) {
                game.cloud_rejected.push(cloud.clone());
            }
            if game.cloud_id.as_deref() == Some(cloud.as_str()) {
                game.cloud_id = None;
                game.cloud_dir = None;
            }
            Ok(json!({ "ok": true }))
        })
        .await
    }

    /// 本机有哪些档案（配对要看的那几栏）。
    async fn local_games(&self) -> Vec<LocalGame> {
        let config = self.config.read().await;
        let mut games: Vec<LocalGame> = config
            .games
            .iter()
            .map(|(id, game)| LocalGame {
                id: id.clone(),
                name: game.name.clone(),
                cloud_id: game.cloud_id.clone(),
                fingerprint: game.exe_fingerprint.clone(),
                // 用 `save_key` 而不是配置里那句人话：跨机器能对上的就是 key（§2.7）。
                locations: game.save_paths.iter().map(crate::sync::save_key).collect(),
                rejected: game.cloud_rejected.clone(),
            })
            .collect();
        games.sort_by(|a, b| a.id.cmp(&b.id));
        games
    }
}

/// 一条配对记录的 JSON：`state` 是界面用来选措辞的数字（见 `pairing::Row::state`）。
fn row_json(row: &crate::sync::pairing::Row, locals: &[LocalGame]) -> Value {
    let name_of = |id: &str| {
        locals
            .iter()
            .find(|local| local.id == id)
            .map(|local| local.name.clone())
            .unwrap_or_default()
    };
    let local = row.local.as_ref();
    json!({
        "cloud_key": row.cloud_key,
        "cloud_id": row.cloud_id,
        "cloud_name": row.cloud_name,
        "machines": row.machines,
        "state": row.state(),
        "local_id": local.map(|l| l.id.clone()).unwrap_or_default(),
        "local_name": local.map(|l| l.name.clone()).unwrap_or_default(),
        "evidence": local.map(|l| l.evidence.as_str()).unwrap_or(""),
        "choices": row
            .candidates
            .iter()
            .map(|id| json!({ "local_id": id, "local_name": name_of(id) }))
            .collect::<Vec<_>>(),
    })
}

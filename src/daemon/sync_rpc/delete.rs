//! 云端存档的删除：删一版 / 清空这一款 / 删掉整条词条（"词条"就是身份卡）。
//!
//! 从 `actions.rs` 拆出来：那边是"上传 / 取回 / 恢复"这一族，而这三个是**破坏性**的，
//! 摆在一起读着才不别扭（也正是它们把 `actions.rs` 顶过了 500 行）。
//!
//! 三者都按**云端落点**收参数：云端有、本机没有的游戏也要能清。每一步都会顺手把**索引**
//! 改对（`note_versions`）—— 详情页的版本列表是实时读桶的，而外层那个「云端存档」列表读
//! 的是本机缓存索引，不改就会一直显示旧版数。

use super::*;

impl Daemon {
    /// 删掉云端这一款的**某一版**。
    ///
    /// ⚠ 收的是**云端落点**（与 `sync.cloud_versions` 同一把尺子）：云端有、本机没有的
    /// 游戏也要能删它的版本，所以这里不收本机 id。
    pub(in crate::daemon) async fn rpc_sync_delete_version(
        &self,
        cloud_key: &str,
        version: &str,
    ) -> Result<Value, String> {
        // 不合形状的版本名是**输入错误**，在碰网络之前就拒掉（与 `sync.restore` 同一套）。
        if !crate::sync::is_snapshot(version) {
            return Err(format!(
                "不是合法的版本名: {version}（形如 20260911T101500Z）"
            ));
        }
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        // 先列一次，为的是"这一版到底在不在"。kopia 的底层删除对找不到的版本是**静默
        // 成功**（那是给自动清理用的 best-effort 语义），用户的显式删除不能走那条路 ——
        // 否则点了删除、其实什么也没删，界面还报成功。
        let before = tokio::time::timeout(CHECK_TIMEOUT, runner.version_infos(cloud_key))
            .await
            .map_err(|_| {
                format!(
                    "列《{cloud_key}》的版本超过 {} 秒没有回应 —— 网络通不通?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;
        let Some(position) = before.iter().position(|info| info.name == version) else {
            return Err(format!("云端没有这一版: {version}"));
        };
        runner
            .remove_version(cloud_key, version)
            .await
            .map_err(|e| e.to_string())?;

        // 索引跟着改：详情页是实时读桶的（删完自然少一版），但外层列表读的是本机索引。
        let rest: Vec<_> = before
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index != position)
            .map(|(_, info)| info)
            .collect();
        let latest = rest.last().map(|info| info.name.clone());
        let size = rest.last().map(|info| info.size).unwrap_or(0);
        self.note_versions(&runner, cloud_key, rest.len(), latest, size, false)
            .await;
        Ok(json!({ "ok": true, "removed": version, "left": rest.len() }))
    }

    /// 删掉云端这一款的**所有存档**（词条留着）。
    ///
    /// 留着词条是有意的："清空存档"不等于"云端不认识这一款" —— 下次同步还能往同一条
    /// 身份上传，配对关系不用重来。
    pub(in crate::daemon) async fn rpc_sync_delete_versions(
        &self,
        cloud_key: &str,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        let removed = runner
            .remove_all_versions(cloud_key)
            .await
            .map_err(|e| e.to_string())?;
        self.note_versions(&runner, cloud_key, 0, None, 0, false)
            .await;
        Ok(json!({ "ok": true, "removed": removed }))
    }

    /// 删掉云端这一款的**词条**（身份卡），**连存档一起**。
    ///
    /// 为什么不只删卡：卡没了、包还在，就是一坨没人认领的孤儿数据（占着桶、谁也不知道
    /// 是谁的）。用户 2026-09-25 拍的板：这一条就是"把这一款从云端彻底抹掉"。
    pub(in crate::daemon) async fn rpc_sync_delete_identity(
        &self,
        cloud_key: &str,
    ) -> Result<Value, String> {
        let settings = self.config.read().await.sync.clone();
        let runner = self.sync_runner(&settings)?;
        // kopia 删卡认的是 `cloud_id`，而这个参数只有落点 —— 从索引里问出来。
        let cloud_id = self
            .cloud_id_of_key(&runner, cloud_key)
            .await
            .ok_or_else(|| format!("索引里没有这一款（{cloud_key}）—— 先刷新一次云端清单再来删"))?;
        let removed = runner
            .remove_all_versions(cloud_key)
            .await
            .map_err(|e| e.to_string())?;
        runner
            .remove_identity(cloud_key, &cloud_id)
            .await
            .map_err(|e| e.to_string())?;
        // 索引里那一条要**整条隐掉**（不是"0 版"）：云端已经不认识它了。
        self.note_versions(&runner, cloud_key, 0, None, 0, true)
            .await;
        Ok(json!({ "ok": true, "removed": removed }))
    }
}

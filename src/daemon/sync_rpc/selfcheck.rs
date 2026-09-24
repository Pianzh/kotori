//! 打开游戏前的自检：接线（纯决策在 [`crate::sync::selfcheck`]）。
//!
//! 这一层只做三件事：把配置读成"这一款现在是什么状态"、需要时才读一次**云端索引**
//! （本机缓存那份，不读身份卡）、把结论落盘。**它绝不拦启动**：读不到就当作"未定"，
//! 用户照样能开始玩。

use serde_json::{Value, json};

use super::Daemon;
use crate::sync::index::IndexGame;
use crate::sync::selfcheck::{CloudPeek, Decision, Found};
use crate::sync::signature::{self, Conclusion};

impl Daemon {
    /// 这一款的当前目标签名；目标没配齐就是 `None`。
    async fn target_signature(&self) -> Option<String> {
        signature::of(&self.config.read().await.sync)
    }

    /// 自检。**只有 `Ask` 会打断用户**（见 `sync::selfcheck`）。
    pub(in crate::daemon) async fn sync_selfcheck(&self, game_id: &str) -> Decision {
        let (game, signature) = {
            let config = self.config.read().await;
            let Some(game) = config.games.get(game_id).cloned() else {
                return Decision::Skip;
            };
            (game, signature::of(&config.sync))
        };

        // 已确认、开关关着、没指纹、没目标：都不用去云端。
        if !crate::sync::selfcheck::needs_cloud(&game, signature.as_deref()) {
            return crate::sync::selfcheck::decide(&game, signature.as_deref(), || Found::None);
        }

        let fingerprint = game.exe_fingerprint.clone().unwrap_or_default();
        let found = self.fingerprint_hit(game_id, &fingerprint).await;
        crate::sync::selfcheck::decide(&game, signature.as_deref(), || found)
    }

    /// 指纹在当前云目标上找到了什么（拿不到索引就当作"没命中"，自检**绝不报错**）。
    ///
    /// ⚠ 读的是**本机缓存里那份索引**，不是所有身份卡 —— 用户 2026-09-24："其他所有查询
    /// 都只查本地索引，最大化减少网络请求次数"。索引是身份卡的镜像，指纹这一栏本来就在
    /// 里面；本地还没有缓存时那条读路径会下载一次（见 `Daemon::cloud_index_view`）。
    async fn fingerprint_hit(&self, game_id: &str, fingerprint: &str) -> Found {
        let view = match self.cloud_index_view(false).await {
            Ok(view) => view,
            Err(error) => {
                tracing::warn!("{game_id}: 自检读不到云端索引，当作未配对: {error}");
                return Found::None;
            }
        };
        // 桶里还没有这份索引（第一次用）：与"云端没有这一款"一样，都是认不出。
        let Some(index) = view.index else {
            return Found::None;
        };
        // 指纹命中**任意一个**即算命中（用户 2026-09-24 的口径）；多条 = 云端自己就有重
        // （同一款被两台机器各建了一次身份）⇒ 要问。
        let hits: Vec<&IndexGame> = index
            .games
            .iter()
            .filter(|game| !fingerprint.is_empty() && game.identity.has_fingerprint(fingerprint))
            .collect();
        let [only] = hits.as_slice() else {
            if hits.is_empty() {
                return Found::None;
            }
            // 命中多条 ⇒ 要问，并把这几条的事实带上（弹窗里显示"最像的那一条"，挑法在
            // `matching::best_like` 里，现在就是取第一条）。
            return Found::Many(hits.iter().map(|game| peek(game)).collect());
        };
        // 这条身份已经被**本机别的档案**认领了 ⇒ 要问：本机不该有两个游戏共用一条身份。
        let taken = {
            let config = self.config.read().await;
            config.games.iter().any(|(id, game)| {
                id != game_id && game.cloud_id.as_deref() == Some(only.identity.cloud_id.as_str())
            })
        };
        if taken {
            // 唯一命中却被占用：这一条照样得让用户看见（"疑似找到"的就是它）。
            return Found::Many(vec![peek(only)]);
        }
        Found::One {
            cloud_id: only.identity.cloud_id.clone(),
            cloud_key: only.cloud_key.clone(),
        }
    }

    /// 把自检的结论落盘（`Skip` / `Pull` / `Ask` 不用落任何东西）。
    pub(in crate::daemon) async fn apply_decision(
        &self,
        game_id: &str,
        decision: &Decision,
    ) -> Result<(), String> {
        let Some(signature) = self.target_signature().await else {
            return Ok(());
        };
        match decision {
            Decision::Adopt {
                cloud_id,
                cloud_key,
            } => {
                self.remember_identity(game_id, cloud_id, cloud_key).await?;
                self.stamp_conclusion(game_id, Conclusion::confirmed(&signature))
                    .await
            }
            // "不再问，直接新建一条身份"：把本机这份身份清掉，上传时就会新建一条。
            // 防错配闸照旧生效 —— 没有身份就取不回任何东西。
            Decision::Fresh => {
                let owner = game_id.to_string();
                self.mutate_config(move |config| {
                    if let Some(game) = config.games.get_mut(&owner) {
                        game.cloud_id = None;
                        game.cloud_dir = None;
                    }
                    Ok(Value::Null)
                })
                .await?;
                self.stamp_conclusion(game_id, Conclusion::confirmed(&signature))
                    .await
            }
            Decision::Skip | Decision::Pull | Decision::Ask { .. } => Ok(()),
        }
    }

    /// 写下"这一款在这个目标上的结论"。
    async fn stamp_conclusion(&self, game_id: &str, value: String) -> Result<(), String> {
        let owner = game_id.to_string();
        self.mutate_config(move |config| {
            if let Some(game) = config.games.get_mut(&owner) {
                game.cloud_conclusion = Some(value);
            }
            Ok(Value::Null)
        })
        .await
        .map(|_| ())
    }

    /// 用户在启动前的对话框里选了什么（`ok` / `off` / `pair`）。
    pub(in crate::daemon) async fn rpc_sync_resolve(
        &self,
        game_id: &str,
        choice: &str,
        cloud_id: Option<&str>,
        cloud_key: Option<&str>,
    ) -> Result<Value, String> {
        let Some(signature) = self.target_signature().await else {
            return Err("云同步还没配好目标（bucket）".to_string());
        };
        match choice {
            // "没问题"：就在这个目标上确认下来，下次不再问。
            "ok" => {
                self.stamp_conclusion(game_id, Conclusion::confirmed(&signature))
                    .await?;
            }
            // "改配对…"：用户挑了一条云端身份（浮层第一项"新建"就是不给 cloud_id）。
            "pair" => {
                match (cloud_id, cloud_key) {
                    (Some(cloud_id), Some(cloud_key)) => {
                        self.remember_identity(game_id, cloud_id, cloud_key).await?
                    }
                    // 新建：清掉本机身份，上传时新建一条。
                    _ => {
                        let owner = game_id.to_string();
                        self.mutate_config(move |config| {
                            if let Some(game) = config.games.get_mut(&owner) {
                                game.cloud_id = None;
                                game.cloud_dir = None;
                            }
                            Ok(Value::Null)
                        })
                        .await?;
                    }
                }
                self.stamp_conclusion(game_id, Conclusion::confirmed(&signature))
                    .await?;
            }
            // "关掉这一款的同步"：只关这一款，而且**记住问过了** —— 他自己再打开时
            // 直接新建身份、不再问（见 `sync::selfcheck`）。
            "off" => {
                let owner = game_id.to_string();
                let value = Conclusion::declined(&signature);
                self.mutate_config(move |config| {
                    let game = config
                        .games
                        .get_mut(&owner)
                        .ok_or_else(|| format!("配置中找不到游戏: {owner}"))?;
                    game.sync_enabled = false;
                    game.cloud_conclusion = Some(value);
                    Ok(Value::Null)
                })
                .await?;
            }
            other => return Err(format!("不认识的回答: {other}")),
        }
        tracing::info!("{game_id}: 启动前自检的回答 {choice}");
        Ok(json!({ "ok": true }))
    }
}

/// 索引里那一条 → 弹窗要显示的那几栏（[`CloudPeek`]）。
///
/// ⚠ 只搬事实、不拼文案：名字与摘要由界面用**同一个函数**生成（见 `CloudPeek` 的注释）。
fn peek(game: &IndexGame) -> CloudPeek {
    CloudPeek {
        cloud_id: game.identity.cloud_id.clone(),
        cloud_key: game.cloud_key.clone(),
        name: game.identity.name.clone(),
        versions: game.versions as u64,
        latest: game.latest.clone().unwrap_or_default(),
        size: game.size,
    }
}

//! 云端索引：一个桶一份的"云端现在有什么"（设计与护栏见 `crate::sync::index`）。
//!
//! 这个模块做五件事：
//!
//!   1. **上传之后**把动过的那一条并进索引（[`Daemon::refresh_index_for`]）；
//!   2. **深度扫描之后**整份重建（[`Daemon::rebuild_index`]，配对扫描顺带做，因为那时
//!      所有身份卡都在手上）；
//!   3. 读出来给「云端存档」页（[`Daemon::rpc_sync_cloud_list`]）；
//!   4. 与本机配置对照：哪一条已配对、哪一条本机没有；
//!   5. **读的时候走本机缓存**（[`Daemon::cloud_index_view`]）：默认零网络，只有用户按
//!      刷新、本地还没有缓存、以及那个每小时的循环（`crate::daemon::index_refresh`）才
//!      真的去云端 —— 用户 2026-09-23："读云端太慢，索引该下载到本地再在本地查"。
//!
//! ⚠ **写入一律"尽力而为"**：索引是加速用的镜像，不是真相（真相是身份卡）。写失败只记
//! 日志 —— **绝不许**让上传失败，包才是要紧的东西。

use std::collections::HashMap;

use serde_json::{Value, json};

use super::rows;
use super::{CHECK_TIMEOUT, Daemon, Runner};
use crate::config::SyncConfig;
use crate::sync::cloud::{CloudGame, GameIdentity};
use crate::sync::index::{CloudIndex, IndexGame};
use crate::sync::index_cache::{self, CACHE_TTL, CachedIndex};

/// 读出来的一份索引 —— 以及它是不是本机缓存里的那一份。
///
/// ⚠ `index: None` 有两层意思，靠 `from_cache` 分开：从缓存来的 `None` 是**确定的**
/// "桶里还没有索引"（拿的时候问过了），联网回来的 `None` 是刚刚问到的同一件事。
/// 界面上那句话都成立："点一次深扫/刷新就有"。
pub(in crate::daemon) struct IndexView {
    pub index: Option<CloudIndex>,
    /// `true` = 这一份来自本机缓存（**这一趟没有打网络**）。
    pub from_cache: bool,
    /// 缓存是什么时候拿下来的（[`crate::sync::index::stamp`] 形状，界面转人话）。
    pub cached_at: String,
    /// 最近一次**刷新**（不管哪条路）失败的原因；`None` = 上一次是好的。
    ///
    /// 用户 2026-09-23 要的："后台失败要让界面知道" —— 界面据此在那句"本机缓存 · X"旁边
    /// 补一句为什么它旧了。
    pub refresh_error: Option<String>,
}

impl Daemon {
    /// 本机缓存里那份索引（按当前目标签名）。读不懂/对不上都当没有（见 `index_cache`）。
    fn cached_index(&self, signature: &str) -> Option<CachedIndex> {
        index_cache::read_at(&crate::config::data_dir(), signature)
    }

    /// 把刚拿到的这一份记进本机缓存。**尽力而为**：写不进去只记日志。
    ///
    /// 写入时机都在"我们刚知道最新内容"这一刻：上传成功、深扫之后、联网读到之后
    /// （见 [`Daemon::cloud_index_view`] 与 [`Daemon::refresh_cached_index`]）。
    fn remember_index(&self, signature: &str, index: Option<CloudIndex>) {
        let cached = CachedIndex::new(signature, index);
        if let Err(error) = index_cache::write_at(&crate::config::data_dir(), &cached) {
            tracing::warn!("云端索引缓存没写成（不影响别的）: {error}");
        }
    }

    /// 记下（或清掉）最近一次刷新失败的原因 —— 读的时候要把它带给界面。
    async fn remember_refresh_error(&self, error: Option<String>) {
        *self.index_refresh_error.write().await = error;
    }

    async fn refresh_error(&self) -> Option<String> {
        self.index_refresh_error.read().await.clone()
    }

    /// 真的去云端读一次索引，顺便更新缓存与"上次刷新失败"那笔账。
    ///
    /// **只有这一处**碰网络：`refresh=true`、缓存过期、本地还没有缓存，三条路都汇到这里。
    async fn fetch_index(
        &self,
        signature: &str,
        settings: &SyncConfig,
    ) -> Result<Option<CloudIndex>, String> {
        let runner = self.sync_runner(settings)?;
        let index = tokio::time::timeout(CHECK_TIMEOUT, runner.read_index())
            .await
            .map_err(|_| {
                format!(
                    "读云端索引超过 {} 秒没有回应 —— 网络通不通?",
                    CHECK_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| e.to_string())?;
        self.remember_index(signature, index.clone());
        Ok(index)
    }

    /// 读云端索引：**默认读本机缓存**，`refresh`、缓存过期、或者本地还没有时才真的去云端。
    ///
    /// 这就是用户 2026-09-23 要的那件事：把那份 JSON 下载到本地，之后在本地查。
    /// 过的三条规矩：
    ///   * 缓存**在一小时以内**就直接给（零网络）；
    ///   * 过了一小时（或者本地没有）就顺手刷一次 —— 用户要的"发现超一小时就刷"；
    ///   * 刷失败时**若有旧缓存就照给**，并把失败原因带上去（页面照旧能用，只是写着"旧"）。
    pub(in crate::daemon) async fn cloud_index_view(
        &self,
        refresh: bool,
    ) -> Result<IndexView, String> {
        let settings = self.config.read().await.sync.clone();
        let signature = crate::sync::signature::of(&settings)
            .ok_or_else(|| "云同步还没配齐：先填 bucket".to_string())?;
        let cached = self.cached_index(&signature);

        // 手上有够新的缓存，又不是用户按的刷新：直接给，一趟网络都不打。
        if !refresh
            && let Some(cached) = &cached
            && !cached.is_stale(CACHE_TTL)
        {
            return Ok(IndexView {
                index: cached.index.clone(),
                from_cache: true,
                cached_at: cached.cached_at.clone(),
                refresh_error: self.refresh_error().await,
            });
        }

        match self.fetch_index(&signature, &settings).await {
            Ok(index) => {
                let cached_at = crate::sync::index::stamp();
                self.remember_refresh_error(None).await;
                let len = index.as_ref().map(CloudIndex::len);
                tracing::debug!(
                    "云端索引已读（{}）: {len:?} 款",
                    if refresh { "刷新" } else { "缓存过期" }
                );
                Ok(IndexView {
                    index,
                    from_cache: false,
                    cached_at,
                    refresh_error: None,
                })
            }
            // 读不成但手上有旧的：**给旧的 + 说清为什么旧**。让整页报错才是更坏的体验
            // （用户要的是"能看见"，而这份索引旧一点并不会做错事）。
            Err(error) if cached.is_some() => {
                tracing::warn!("读云端索引失败（先给本机缓存）: {error}");
                self.remember_refresh_error(Some(error)).await;
                let cached = cached.expect("上面判过 is_some");
                Ok(IndexView {
                    index: cached.index,
                    from_cache: true,
                    cached_at: cached.cached_at,
                    refresh_error: self.refresh_error().await,
                })
            }
            // 连旧缓存都没有：如实报错（界面那句是"没问成"，不是"云端没有"）。
            Err(error) => {
                self.remember_refresh_error(Some(error.clone())).await;
                Err(error)
            }
        }
    }

    /// 联网读一次索引、只更新缓存。**不扫身份卡、不碰配对表**。
    ///
    /// 三个调用点：上面那条读路径（缓存过期 / 本地没有）、每小时的循环、改完同步设置之后
    /// （见 `crate::daemon::index_refresh`）。没配齐或拿不到凭据时安静跳过 —— 没配云同步的
    /// 人不该被这个循环刷屏。
    pub(in crate::daemon) async fn refresh_cached_index(&self, why: &str) {
        let settings = self.config.read().await.sync.clone();
        let Some(signature) = crate::sync::signature::of(&settings) else {
            return;
        };
        match self.fetch_index(&signature, &settings).await {
            Ok(index) => {
                let count = index.as_ref().map(CloudIndex::len);
                self.remember_refresh_error(None).await;
                tracing::debug!("云端索引已刷新（{why}）: {count:?} 款");
            }
            Err(error) => {
                // 拿不到凭据/没装引擎那种"配置问题"不该每小时刷一次日志，但用户要求在界面上
                // 看得见 —— 所以账记着（界面会显示），日志按 warn 留一条就够。
                tracing::warn!("云端索引刷新失败（{why}）: {error}");
                self.remember_refresh_error(Some(error)).await;
            }
        }
    }
}

impl Daemon {
    /// 这一款现在该记成索引里的哪一条。
    ///
    /// 只用**本机配置 + 云端版本名列表**，**不读身份卡**：别的机器记下的指纹/位置由
    /// [`Runner::update_index`] 合并时保留，不必我们再去读一遍。
    async fn index_entry(
        &self,
        runner: &Runner,
        game_id: &str,
        cloud_key: &str,
    ) -> Result<IndexGame, String> {
        let (cloud_id, name) = {
            let config = self.config.read().await;
            let game = config
                .games
                .get(game_id)
                .ok_or_else(|| format!("配置中找不到游戏: {game_id}"))?;
            (
                game.cloud_id
                    .clone()
                    .ok_or_else(|| format!("{game_id} 还没有云端身份"))?,
                game.name.clone(),
            )
        };

        let versions = runner
            .packages(cloud_key)
            .await
            .map_err(|e| e.to_string())?;
        let latest = versions.last().cloned();

        let mut identity = GameIdentity::new(&cloud_id, &name);
        identity.merge_machine(self.machine_identity_of(game_id).await?);
        let mut game = IndexGame::from_identity(cloud_key, identity);
        // 大小那一刀（§12 D2）再填；现在如实当"不知道"。
        game.versions = versions.len();
        game.latest = latest;
        Ok(game)
    }

    /// 上传成功之后把这一条并进索引。**尽力而为**：写不进去只记日志。
    ///
    /// 调用点在"这一版已经上云了"之后 —— 索引没记上顶多让「云端存档」页晚一步看见它，
    /// 而让上传报错会让人以为存档没保住。
    pub(super) async fn refresh_index_for(&self, runner: &Runner, game_id: &str) {
        let cloud_key = match self.cloud_key_of(game_id).await {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上（拿不到云端落点）: {error}");
                return;
            }
        };
        let entry = match self.index_entry(runner, game_id, &cloud_key).await {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上: {error}");
                return;
            }
        };
        let machine_id = match self.machine_id().await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!("{game_id}: 索引没记上（拿不到机器 id）: {error}");
                return;
            }
        };
        match runner.update_index(&machine_id, vec![entry]).await {
            Ok(index) => {
                // 刚写过云端那份，就顺手把本机缓存也更新掉 —— 下一次读连网络都省了
                // （用户 2026-09-23："每次同步自动更新"）。
                self.remember_index_for_current_target(&index).await;
                tracing::debug!("{game_id}: 索引已更新（云端 {} 款）", index.len());
            }
            Err(error) => tracing::warn!("{game_id}: 索引没记上: {error}"),
        }
    }

    /// 用当前同步设置算出签名，把这份索引记进本机缓存（上传成功/深扫之后用）。
    ///
    /// 算不出签名（没配 bucket）就什么都不做 —— 那时也没有"这个目标的缓存"可言。
    async fn remember_index_for_current_target(&self, index: &CloudIndex) {
        let settings = self.config.read().await.sync.clone();
        if let Some(signature) = crate::sync::signature::of(&settings) {
            self.remember_index(&signature, Some(index.clone()));
            self.remember_refresh_error(None).await;
        }
    }

    /// 深度扫描之后整份重建索引：身份卡是真相，索引照它重写一份。
    ///
    /// 顺带把每款的版本数问出来（rclone 是每款列一次目录、kopia 一次列全部）——
    /// 这条路本来就是"慢路"，多这一下不改变什么。
    pub(super) async fn rebuild_index(&self, runner: &Runner, cards: &[(String, GameIdentity)]) {
        let summaries: HashMap<String, CloudGame> = match runner.cloud_games().await {
            Ok(games) => games
                .into_iter()
                .map(|game| (game.id.clone(), game))
                .collect(),
            Err(error) => {
                tracing::warn!("索引重建: 列版本摘要失败（先记 0）: {error}");
                HashMap::new()
            }
        };
        let changes: Vec<IndexGame> = cards
            .iter()
            .map(|(key, identity)| {
                let mut game = IndexGame::from_identity(key, identity.clone());
                if let Some(summary) = summaries.get(key) {
                    game.set_summary(summary.versions, summary.latest.clone(), summary.size);
                }
                game
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let machine_id = match self.machine_id().await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!("索引重建失败（拿不到机器 id）: {error}");
                return;
            }
        };
        match runner.update_index(&machine_id, changes).await {
            Ok(index) => {
                // 深扫之后索引是照身份卡整份重写的 —— 顺手把本机缓存也换成这一份。
                self.remember_index_for_current_target(&index).await;
                tracing::info!("云端索引已重建：{} 款", index.len());
            }
            Err(error) => tracing::warn!("索引重建失败: {error}"),
        }
    }

    /// `sync.cloud_list`：云端现在有哪些游戏（读**本机缓存**，不读身份卡）。
    ///
    /// 回包里每一行都带上"本机哪一条与它配对" —— 那是本地信息（比对 `cloud_id`）。
    /// `refresh: true` 是「云端存档」页那颗刷新按钮（强制联网）；其余情况走缓存，缓存过了一
    /// 小时才顺手刷一次（见 [`Daemon::cloud_index_view`]）。另外回包带上这份清单是**什么时候
    /// 拿到的**（`cached_at`）与上次刷新失败的原因（`refresh_error`）—— 用户 2026-09-23：
    /// 界面要显示时间，后台失败也要让他知道。
    ///
    /// `indexed: false` 表示桶里还没建过索引（老桶、或索引被删了），此时 `games` 是空的，
    /// 界面该提示去点「深度扫描云端」。
    pub(in crate::daemon) async fn rpc_sync_cloud_list(
        &self,
        refresh: bool,
    ) -> Result<Value, String> {
        let view = self.cloud_index_view(refresh).await?;
        let index = view.index;

        // 本机这一侧：哪个身份被哪一条档案认了（本地查表，不碰网络）。
        let (paired, rejected) = {
            let config = self.config.read().await;
            rows::locals(&config)
        };

        let indexed = index.is_some();
        let games: Vec<Value> = index
            .unwrap_or_else(CloudIndex::new)
            .games
            .iter()
            .map(|game| {
                let cloud_id = game.identity.cloud_id.clone();
                // 本机明确否过这一条（配对表那笔账）：界面上要能说"你之前说了不是它"。
                rows::game_json(game, paired.get(&cloud_id), rejected.contains(&cloud_id))
            })
            .collect();

        Ok(json!({
            "indexed": indexed,
            "from_cache": view.from_cache,
            "cached_at": view.cached_at,
            "refresh_error": view.refresh_error,
            "games": games,
        }))
    }
}

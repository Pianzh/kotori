//! 云端索引的**每小时自动刷新**（用户 2026-09-23 定的）。
//!
//! 索引平时只在三种时候变：用户按「云端存档」页的刷新、上传成功、深扫之后。加这一个循环
//! 是为了让本地缓存别太旧 —— 用户的原话是"每隔一个小时自动下载同步一次"，代价是一小时
//! 一趟网络（rclone 两个对象、kopia 一条 restore），换"打开页面就有"。
//!
//! ⚠ 它**只更新缓存**：不扫身份卡、不碰配对表、不绑任何东西、不改配置。要不要相信这份
//! 索引是别的路径的事（而且认人还有指纹那一关）。
//! ⚠ 读路径（打开页面 / 「自己选…」/ 添加页匹配）平时只读本地；**只有缓存比 [`CACHE_TTL`]
//! 还旧**、本地还没有缓存、或者用户按了「云端存档」页的刷新时，才会真的去云端。

use std::time::Duration;

use super::*;
use crate::sync::index_cache::CACHE_TTL;

/// 多久自动刷一次。与"缓存多旧算旧"是**同一个数**（见 `index_cache::CACHE_TTL`）：
/// 循环刚把缓存刷新，读路径就绝不会因为过期而再来一次。
pub(super) const INDEX_REFRESH_INTERVAL: Duration = CACHE_TTL;

impl Daemon {
    /// 起这个循环。`run()` 里跟在 `spawn_process_watch` 后面调一次。
    pub(super) fn spawn_index_refresh(&self) {
        let this = self.clone_shares();
        tokio::spawn(async move {
            // **起来先刷一次**（用户 2026-09-23 定的）：关机一周再开机时，缓存里那份是
            // 一周前的，而用户多半开机就会打开界面 —— 与其让他先看到一份旧清单，不如
            // 开机就把它换掉。之后每小时一次。
            this.refresh_cached_index("启动时").await;
            loop {
                tokio::time::sleep(INDEX_REFRESH_INTERVAL).await;
                this.refresh_cached_index("每小时一次").await;
            }
        });
    }

    /// 改完同步设置之后顺手刷一次（**不阻塞那一次保存**）。
    ///
    /// 为什么要它：设置一改，目标签名就变了（换桶、换引擎、换 prefix），新签名在本机还
    /// 没有缓存 —— 于是下一次打开页面会为"其实刚配好"这件事白等一趟。顺手刷一次，把新
    /// 目标的缓存直接建起来。用户 2026-09-23 定的："改同步设置之后默认刷新一次缓存"。
    ///
    /// `touches_target` 由调用方算（`SettingsPatch` 的字段是它自己的事）：**只改了保留版本
    /// 数、程序位置**那种与目标无关的字段时，本机那份缓存还是对的，不必刷。
    ///
    /// ⚠ **后台做**：那条 RPC 的回话是"设置已保存"，不该因为一趟网络让人以为没保存。
    pub(super) fn refresh_index_after_settings(&self, touches_target: bool) {
        if !touches_target {
            return;
        }
        let this = self.clone_shares();
        tokio::spawn(async move {
            this.refresh_cached_index("改完同步设置").await;
        });
    }
}

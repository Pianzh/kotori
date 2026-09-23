//! 「云端存档」这一页的消息：读索引、深度扫描、搜索、点开看版本。
//!
//! 从 `update/sync.rs` 拆出来：那边的设置与凭据本身就是一整块，而这一块**只读云端**、
//! 一个字都不改本机配置 —— 语义上就该分开。
//!
//! 两条读路径的代价差得很远，所以分成两颗按钮：
//!   * 「刷新」读**索引**（一次读，便宜）；
//!   * 「深度扫描云端」读**所有身份卡**并重建索引（kopia 那边一张卡一次 `restore`，慢）。

use super::super::*;

impl App {
    /// 切到「云端存档」时该做什么：**第一次进来**读一次索引（一次读，便宜）。
    ///
    /// 只读一次：之后的刷新由用户自己按 —— 读云端这件事不该跟着切页面反复发生。
    pub(super) fn cloud_entered(&mut self) -> Task<Message> {
        if self.cloud.loaded_once {
            return Task::none();
        }
        self.cloud.loading = true;
        self.cloud.msg = Some("正在读云端索引…".to_string());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { cloud_list(&socket).await },
            Message::CloudLoaded,
        )
    }

    /// update_cloud 负责的那一批消息（路由见 `update/mod.rs`）。
    pub(super) fn update_cloud(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloudRefresh => {
                self.cloud.loading = true;
                self.cloud.msg = Some("正在读云端索引…".to_string());
                self.cloud.ok = true;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { cloud_list(&socket).await },
                    Message::CloudLoaded,
                )
            }
            Message::CloudLoaded(result) => {
                self.cloud.loading = false;
                self.cloud.scanning = false;
                match result {
                    Ok((indexed, rows)) => {
                        self.cloud.msg = Some(cloud_summary(indexed, rows.len()));
                        self.cloud.ok = true;
                        self.cloud.loaded(indexed, rows);
                    }
                    Err(e) => {
                        self.cloud.ok = false;
                        self.cloud.msg = Some(format!("读云端索引失败: {e}"));
                    }
                }
                Task::none()
            }
            Message::CloudScan => {
                self.cloud.scanning = true;
                self.cloud.msg =
                    Some("正在深度扫描云端（要读每一张身份卡，可能会慢）…".to_string());
                self.cloud.ok = true;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { cloud_scan(&socket).await },
                    Message::CloudScanned,
                )
            }
            Message::CloudScanned(result) => {
                self.cloud.scanning = false;
                match result {
                    Ok((indexed, rows)) => {
                        let count = rows.len();
                        self.cloud.ok = true;
                        self.cloud.loaded(indexed, rows);
                        // 深度扫描还会顺手把指纹唯一命中的绑上（`sync.pairing` 干的），
                        // 所以这句话里要提一句"配对了没有"。
                        self.cloud.msg = Some(match self.cloud.matched() {
                            0 => format!("扫描完成：云端 {count} 款，没有新配上的。"),
                            bound => {
                                format!("扫描完成：云端 {count} 款，其中 {bound} 款已配上本机。")
                            }
                        });
                    }
                    Err(e) => {
                        self.cloud.ok = false;
                        self.cloud.msg = Some(format!("深度扫描失败: {e}"));
                    }
                }
                Task::none()
            }
            Message::CloudSearch(text) => {
                // 本地过滤：不打网络，也不动已经读到的清单。
                self.cloud.search = text;
                Task::none()
            }
            Message::CloudToggle(key) => {
                // 收起来不用问云端 —— 这个判断在模型里（`CloudState::toggle`），有单测。
                if !self.cloud.toggle(&key) {
                    return Task::none();
                }
                let socket = self.daemon_socket.clone();
                let asked = key.clone();
                Task::perform(
                    async move { cloud_versions(&socket, asked).await },
                    move |result| Message::CloudVersionsLoaded(key, result),
                )
            }
            Message::CloudVersionsLoaded(key, result) => {
                match result {
                    Ok(versions) => self.cloud.versions_loaded(&key, versions),
                    // 挂在哪一款上由模型判（点开甲又点开乙时，甲的回包要丢掉）。
                    Err(e) => self
                        .cloud
                        .versions_failed(&key, format!("列《{key}》的版本失败: {e}")),
                }
                Task::none()
            }
            Message::CloudBack => {
                self.cloud.back();
                Task::none()
            }
            // 委派是按变体名精确列的：漏一个就会走到这里，测试会立刻炸。
            other => unreachable!("update_cloud 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

/// 清单读完那句话说清"索引建过没有" —— 那两件事在界面上完全不同。
fn cloud_summary(indexed: bool, count: usize) -> String {
    match (indexed, count) {
        (false, _) => "桶里还没有这份索引。点「深度扫描云端」扫一次，之后刷新就快了。".to_string(),
        (true, 0) => "云端还没有游戏。".to_string(),
        (true, count) => format!("云端 {count} 款游戏。点一款看它每一版。"),
    }
}

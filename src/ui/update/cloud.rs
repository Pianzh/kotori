//! 「云端存档」那一块的消息：列云端有哪些游戏、点开一款看它每一版。
//!
//! 从 `update/sync.rs` 拆出来：那边的设置与凭据本身就是一整块，而这一块**只读
//! 云端**、一个字都不改本机配置 —— 语义上就该分开，顺带让两边都读得完。

use super::super::*;

impl App {
    /// update_cloud 负责的那一批消息（路由见 `update/mod.rs`）。
    ///
    /// 拆出来只是因为 `update` 那个 match 太长：**这里改的仍然是同一个 `App`**。
    pub(super) fn update_cloud(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SyncCloudRefresh => {
                self.cloud.loading = true;
                self.cloud.msg = Some("正在列云端…".to_string());
                self.cloud.ok = true;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_cloud_games(&socket).await },
                    Message::SyncCloudLoaded,
                )
            }
            Message::SyncCloudLoaded(result) => {
                self.cloud.loading = false;
                match result {
                    Ok(rows) => {
                        let versions: usize = rows.iter().map(|row| row.versions).sum();
                        self.cloud.msg = Some(if rows.is_empty() {
                            "云端还没有游戏。".to_string()
                        } else {
                            format!(
                                "云端 {} 款游戏，共 {versions} 版存档。点一款看它每一版。",
                                rows.len()
                            )
                        });
                        self.cloud.ok = true;
                        self.cloud.loaded(rows);
                    }
                    Err(e) => {
                        self.cloud.ok = false;
                        self.cloud.msg = Some(format!("列云端失败: {e}"));
                    }
                }
                Task::none()
            }
            Message::SyncCloudToggle(key) => {
                // 收起来不用问云端 —— 这个判断在模型里（`CloudBoard::toggle`），有单测。
                if !self.cloud.toggle(&key) {
                    return Task::none();
                }
                let socket = self.daemon_socket.clone();
                let asked = key.clone();
                Task::perform(
                    async move { sync_cloud_versions(&socket, asked).await },
                    move |result| Message::SyncCloudVersionsLoaded(key, result),
                )
            }
            Message::SyncCloudVersionsLoaded(key, result) => {
                match result {
                    Ok(versions) => self.cloud.versions_loaded(&key, versions),
                    // 挂在哪一款上由模型判（点开甲又点开乙时，甲的回包要丢掉）。
                    Err(e) => self
                        .cloud
                        .versions_failed(&key, format!("列《{key}》的版本失败: {e}")),
                }
                Task::none()
            }
            // 委派是按变体名精确列的：漏一个就会走到这里，测试会立刻炸。
            other => unreachable!("update_cloud 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

//! 「云端存档」这一页的消息：读索引、深度扫描、搜索、点开看版本。
//!
//! 从 `update/sync.rs` 拆出来：那边的设置与凭据本身就是一整块，而这一块**只跟云端打交道**、
//! 一个字都不改本机配置 —— 语义上就该分开。
//!
//! ⚠ 这一页对云端是**能删的**（用户 2026-09-26 定了分工：这里是"管理云端的工具"，平等处理
//! 云端所有游戏，不关心本机有没有装上它）：清单里点开某一款 → 看它每一版 → 每一版再点进去
//! 就是一个存档的管理页（现在只有删除）。"本机 ↔ 云端"的交互（上传 / 取回 / 覆盖本机）不在
//! 这里，在单游戏设置那页（`update/versions.rs`）。
//!
//! 两条读路径的代价差得很远，所以分成两颗按钮：
//!   * 「刷新」读**索引**（一次读，便宜）；
//!   * 「深度扫描云端」读**所有身份卡**并重建索引（kopia 那边一张卡一次 `restore`，慢）。

use super::super::*;

impl App {
    /// 切到「云端存档」时该做什么：**第一次进来**读一次索引（一次读，便宜）。
    ///
    /// 只读一次：之后的刷新由用户自己按 —— 读云端这件事不该跟着切页面反复发生。
    /// 读的是**本机缓存**（`refresh = false`）：平时这一步根本不碰网络，只有缓存过了一小时
    /// 或者本地还没有缓存时才会真的去云端（daemon 那边判，见 `cloud_index_view`）。
    pub(super) fn cloud_entered(&mut self) -> Task<Message> {
        if self.cloud.loaded_once {
            return Task::none();
        }
        self.cloud.loading = true;
        self.cloud.msg = Some("正在读本机那份云端清单…".to_string());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { cloud_list(&socket, false).await },
            Message::CloudLoaded,
        )
    }

    /// update_cloud 负责的那一批消息（路由见 `update/mod.rs`）。
    pub(super) fn update_cloud(&mut self, message: Message) -> Task<Message> {
        match message {
            // 这一颗是**唯一**会强制联网的读（用户 2026-09-23："除了云端存档标签页的刷新
            // 以外其他地方都不会触发刷新缓存"）。
            Message::CloudRefresh => {
                self.cloud.loading = true;
                self.cloud.msg = Some("正在从云端读索引…".to_string());
                self.cloud.ok = true;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { cloud_list(&socket, true).await },
                    Message::CloudLoaded,
                )
            }
            Message::CloudLoaded(result) => {
                self.cloud.loading = false;
                self.cloud.scanning = false;
                match result {
                    Ok(reply) => {
                        self.cloud.msg = Some(cloud_summary(&reply));
                        self.cloud.ok = true;
                        self.cloud.loaded(reply);
                        // 清单整份换过了：两层详情都可能已经不成立。
                        self.cloud_version.closed();
                    }
                    Err(e) => {
                        self.cloud.ok = false;
                        self.cloud.msg = Some(format!("读云端清单失败: {e}"));
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
                    Ok(reply) => {
                        let count = reply.rows.len();
                        let source = reply.source_label();
                        let trouble = reply.trouble_label();
                        self.cloud.ok = true;
                        self.cloud.loaded(reply);
                        self.cloud_version.closed();
                        // 深度扫描还会顺手把指纹唯一命中的绑上（`sync.pairing` 干的），
                        // 所以这句话里要提一句"配对了没有"。
                        let mut message = match self.cloud.matched() {
                            0 => format!("扫描完成：云端 {count} 款，没有新配上的。"),
                            bound => {
                                format!("扫描完成：云端 {count} 款，其中 {bound} 款已配上本机。")
                            }
                        };
                        message.push_str(&format!("\n{source}"));
                        if let Some(trouble) = trouble {
                            message.push_str(&format!("\n{trouble}"));
                        }
                        self.cloud.msg = Some(message);
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
            Message::CloudOpenGame(key) => {
                // 值不值得问云端由模型判（重复点同一款不再问），有单测。
                if !self.cloud.open(&key) {
                    return Task::none();
                }
                // 换一款就把再下一层那个"某一个存档"的页面收掉（它属于上一款）。
                self.cloud_version.closed();
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
                self.cloud_version.closed();
                Task::none()
            }
            // ── 这一款详情页底部那两颗"整款"按钮（纯云上的管理，不碰本机） ──
            Message::CloudDeleteVersions => {
                self.cloud.delete_requested(Confirmation::ClearVersions);
                Task::none()
            }
            Message::CloudDeleteIdentity => {
                self.cloud.delete_requested(Confirmation::ForgetIdentity);
                Task::none()
            }
            Message::CloudDeleteCancelled => {
                self.cloud.cancelled();
                Task::none()
            }
            Message::CloudDeleteConfirmed => {
                let Some(action) = self.cloud.confirmed() else {
                    return Task::none();
                };
                // 两颗按钮都按**点开的那一款**动手；列表上按不到它们（弹窗只在详情里）。
                let Some(key) = self.cloud.open.clone() else {
                    return Task::none();
                };
                let socket = self.daemon_socket.clone();
                match action {
                    Confirmation::ClearVersions => Task::perform(
                        async move { sync_clear_versions(&socket, key).await },
                        Message::CloudDeleted,
                    ),
                    Confirmation::ForgetIdentity => Task::perform(
                        async move { sync_forget_identity(&socket, key).await },
                        Message::CloudDeleted,
                    ),
                    // 别的动作不会从这一页发出来（`delete_requested` 只接受这两个）。
                    _ => unreachable!("这一页只发得出那两颗\"整款\"删除"),
                }
            }
            Message::CloudDeleted(result) => {
                self.cloud.deleted(result);
                Task::none()
            }
            // ── 再下一层：一个存档的管理页（现在只有删除） ──
            Message::CloudVersionOpened(version) => {
                // 那一版得在手上这份列表里（页面上的每一个条目都来自它）；找不到就当没点。
                let Some(row) = self
                    .cloud
                    .versions
                    .iter()
                    .find(|row| row.name == version)
                    .cloned()
                else {
                    return Task::none();
                };
                let (Some(game_name), Some(key)) = (
                    self.cloud.opened().map(|row| row.name.clone()),
                    self.cloud.open.clone(),
                ) else {
                    return Task::none();
                };
                self.cloud_version.opened(&game_name, &key, &row);
                Task::none()
            }
            Message::CloudVersionClosed => {
                self.cloud_version.closed();
                Task::none()
            }
            Message::CloudVersionDeleteRequested => {
                self.cloud_version.requested();
                Task::none()
            }
            Message::CloudVersionCancelled => {
                self.cloud_version.cancelled();
                Task::none()
            }
            Message::CloudVersionConfirmed => {
                let Some((key, version)) = self.cloud_version.confirmed() else {
                    return Task::none();
                };
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_delete_version(&socket, key, version).await },
                    Message::CloudVersionDeleted,
                )
            }
            Message::CloudVersionDeleted(result) => {
                // 成了：那一页自己收掉，结果写在外层那一页上（用户被送回去时看得见）。
                if let Some((version, summary)) = self.cloud_version.deleted(result) {
                    self.cloud.forget_version(&version, summary);
                }
                Task::none()
            }
            // 委派是按变体名精确列的：漏一个就会走到这里，测试会立刻炸。
            other => unreachable!("update_cloud 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

/// 清单读完那句话说三件事：索引建过没有、有几款、**这份清单是什么时候拿到的**。
///
/// 最后那件是用户 2026-09-23 要的：索引平时读的是本机缓存，不写清时间用户会以为"云端就
/// 长这样"，而它可能已经旧了一小时。后台刷新失败的原因也在同一句里说（免得用户只看到一份
/// 旧清单而不知道原因）。
fn cloud_summary(reply: &CloudListReply) -> String {
    let mut message = match (reply.indexed, reply.rows.len()) {
        (false, _) => "桶里还没有这份索引。点「深度扫描云端」扫一次，之后刷新就快了。".to_string(),
        (true, 0) => "云端还没有游戏。".to_string(),
        (true, count) => format!("云端 {count} 款游戏。点一款看它每一版。"),
    };
    let source = reply.source_label();
    if !source.is_empty() {
        message.push_str(&format!("\n{source}"));
    }
    if let Some(trouble) = reply.trouble_label() {
        message.push_str(&format!("\n{trouble}"));
    }
    message
}

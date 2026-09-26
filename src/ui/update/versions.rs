//! 单游戏页那一页「这一款的云端存档」：列版本、挑一版替换本机、删一版、清空这一款、
//! 把这一款从云端抹掉。
//!
//! 与 `update/cloud.rs`（那一页看的是"云端都有什么"，只读）分开：这一页能做的事都是
//! **破坏性**的（覆盖本机存档目录、删云端的东西），所以走"先记下待确认的那件事、确认之后
//! 才发出去"那条路 —— 四种动作共用同一个弹窗（`Confirmation` 说清是哪一件），与云同步
//! 页那颗「恢复」同一个形状。

use super::super::*;

impl App {
    /// 点了身份那一条（整条可点）：开页，并按当前这一款去列它那条身份的版本。
    ///
    /// 还没绑定身份（算不出 `cloud_key`）就只开页、一个请求都不发 —— 空态那句话由
    /// `bound` 说。用户 2026-09-25：没绑定画不画都行，列不出东西也是正常的。
    pub(super) fn versions_opened(&mut self) -> Task<Message> {
        let Some(game_id) = self.selected.clone() else {
            return Task::none();
        };
        let key = self
            .selected_sync_game()
            .map(|row| row.cloud_key.clone())
            .unwrap_or_default();
        self.versions.opened(&game_id, &key);
        if key.is_empty() {
            return Task::none();
        }
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { cloud_versions(&socket, key).await },
            Message::GameVersionsLoaded,
        )
    }

    /// update_versions 负责的那一批消息（路由见 `update/mod.rs`）。
    pub(super) fn update_versions(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::GameVersionsOpened => self.versions_opened(),
            Message::GameVersionsClosed => {
                self.versions.closed();
                Task::none()
            }
            Message::GameVersionsLoaded(result) => {
                match result {
                    Ok(rows) => self.versions.loaded(rows),
                    Err(e) => self.versions.failed(format!("读云端版本失败: {e}")),
                }
                Task::none()
            }
            // 每颗按钮只记下要问哪一件事：真正的动作等弹窗那一下。
            Message::GameVersionsReplaceVersion(version) => {
                self.versions.requested(Confirmation::Replace { version });
                Task::none()
            }
            Message::GameVersionsDeleteVersion(version) => {
                self.versions
                    .requested(Confirmation::DeleteVersion { version });
                Task::none()
            }
            Message::GameVersionsClearVersions => {
                self.versions.requested(Confirmation::ClearVersions);
                Task::none()
            }
            Message::GameVersionsForgetIdentity => {
                self.versions.requested(Confirmation::ForgetIdentity);
                Task::none()
            }
            Message::GameVersionsCancelled => {
                self.versions.cancelled();
                Task::none()
            }
            // 四种动作共用这一个弹窗：分派按状态里记着的那件事走。
            Message::GameVersionsConfirmed => {
                let Some(action) = self.versions.confirmed() else {
                    return Task::none();
                };
                let key = self.versions.cloud_key.clone();
                let game_id = self.versions.game_id.clone();
                let socket = self.daemon_socket.clone();
                match action {
                    Confirmation::Replace { version } => Task::perform(
                        async move { sync_restore(&socket, &game_id, Some(version.as_str())).await },
                        Message::GameVersionsReplaced,
                    ),
                    Confirmation::DeleteVersion { version } => Task::perform(
                        async move { sync_delete_version(&socket, key, version).await },
                        Message::GameVersionsDeleted,
                    ),
                    Confirmation::ClearVersions => Task::perform(
                        async move { sync_clear_versions(&socket, key).await },
                        Message::GameVersionsDeleted,
                    ),
                    Confirmation::ForgetIdentity => Task::perform(
                        async move { sync_forget_identity(&socket, key).await },
                        Message::GameVersionsDeleted,
                    ),
                }
            }
            // ⚠ 结果写在这一页自己那句话上（`versions.replaced`），不是云同步页那句。
            Message::GameVersionsReplaced(result) => {
                self.versions.replaced(result);
                Task::none()
            }
            Message::GameVersionsDeleted(result) => {
                self.versions.deleted(result);
                // 删完顺手把状态重读一次：身份那行摘要（"N 版"）是从 `sync.status` 来的，
                // 不重读就会在刚删空的列表下面挂着旧数字。
                Task::perform(async { load_sync_status().await }, |result| {
                    Message::SyncStatusLoaded(Box::new(result))
                })
            }
            other => unreachable!("update_versions 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

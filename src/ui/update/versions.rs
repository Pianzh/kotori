//! 单游戏页那一页「这一款的云端存档」：列版本、挑一版替换本机。
//!
//! 与 `update/cloud.rs`（那一页看的是"云端都有什么"，只读）分开：这一页能做的事是
//! **破坏性**的（覆盖本机存档目录），所以走"先记下待确认的那一版、确认之后才发出去"
//! 那条路 —— 与云同步页那颗「恢复」同一个形状。

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
            // 「替换」只记下是哪一版：真正的动作等弹窗那一下。
            Message::GameVersionsReplace(version) => {
                self.versions.requested(&version);
                Task::none()
            }
            Message::GameVersionsReplaceCancelled => {
                self.versions.cancelled();
                Task::none()
            }
            Message::GameVersionsReplaceConfirmed => {
                let Some(version) = self.versions.confirmed() else {
                    return Task::none();
                };
                let game_id = self.versions.game_id.clone();
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_restore(&socket, &game_id, Some(version.as_str())).await },
                    Message::GameVersionsReplaced,
                )
            }
            // ⚠ 结果写在这一页自己那句话上（`versions.done`），不是云同步页那句。
            Message::GameVersionsReplaced(result) => {
                self.versions.done(result);
                Task::none()
            }
            other => unreachable!("update_versions 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

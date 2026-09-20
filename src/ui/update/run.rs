//! 「启动 / 停止」那一颗按钮。
//!
//! 从 `update/mod.rs` 拆出来(那边又贴着 500 行):这一族说的是同一件事 —— **一颗
//! 按钮两种时候**,而"现在该哪一种"只有一处判断。

use super::super::*;

/// 那一颗「启动 / 停止」按钮**该干什么**。
///
/// ⚠ 这件事从前在界面上判了两次:库行那颗按钮按 `state` 分成两条路,而详情页头部
/// 那颗**永远发 `game.launch`** —— 于是它在游戏跑起来之后显示成「停止」,点下去还在
/// 启动(用户 2026-09-20 实测:"本质还是启动,没有用")。现在文案与动作出自同一个
/// 判断:都看会话表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum RunAction {
    Launch,
    Stop,
}

/// 会话表里有它 → 停;没有 → 启动。
pub(in crate::ui) fn run_action(
    running: &std::collections::BTreeMap<String, SessionInfo>,
    game_id: &str,
) -> RunAction {
    if running.contains_key(game_id) {
        RunAction::Stop
    } else {
        RunAction::Launch
    }
}

impl App {
    /// 那一颗「启动 / 停止」按钮被点了。
    pub(super) fn toggle_run(&mut self, game_id: String) -> Task<Message> {
        // 启动已经在路上时按钮本来就是灰的;真到了这儿也别再发一次(那一发会开出
        // 第二个会话)。
        if self.launching.as_deref() == Some(game_id.as_str()) {
            return Task::none();
        }
        match run_action(&self.running, &game_id) {
            RunAction::Launch => self.launch_game(game_id),
            RunAction::Stop => self.stop_game(game_id),
        }
    }

    /// 启动一款游戏:`launching` 立刻立起来(按钮随即变成灰的),等 daemon 回话。
    fn launch_game(&mut self, id: String) -> Task<Message> {
        self.launching = Some(id.clone());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move {
                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(id));
                crate::rpc::call(&socket, "game.launch", Some(params)).await
            },
            Message::LaunchDone,
        )
    }

    /// 停掉这一款正在跑的会话。会话表里没有它就什么都不做(界面本该是灰的)。
    fn stop_game(&mut self, game_id: String) -> Task<Message> {
        let Some(session) = self.running.get(&game_id).map(|s| s.session_id.clone()) else {
            return Task::none();
        };
        let socket = self.daemon_socket.clone();
        self.error = None;
        Task::perform(
            async move { stop_session(&socket, &session).await },
            Message::StopDone,
        )
    }
}

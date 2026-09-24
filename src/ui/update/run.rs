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
            RunAction::Stop => {
                // ⚠ 「停止」会**真的把这一局结束掉**(Linux 杀进程组、Windows 杀整棵
                // 树),所以会杀进程的那一局先问一次(用户 2026-09-20:"建议对停止追加
                // 确认")。观测会话点一下就停 —— 那只是"别再跟着它",不必吓人
                // (见 `App::stop_needs_confirm`)。
                if self.stop_needs_confirm(&game_id) && !self.confirm_stop {
                    self.confirm_stop = true;
                    return Task::none();
                }
                self.stop_game(game_id)
            }
        }
    }

    /// 点下去会不会**杀进程**?会才值得二次确认。
    fn stop_needs_confirm(&self, game_id: &str) -> bool {
        self.running
            .get(game_id)
            .is_some_and(|session| !session.watch_only)
    }

    /// 启动一款游戏:`launching` 立刻立起来(按钮随即变成灰的),等 daemon 回话。
    fn launch_game(&mut self, id: String) -> Task<Message> {
        // 上一款留下的那次"等确认"不许落到这一款身上。
        self.confirm_stop = false;
        self.launching = Some(id.clone());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move {
                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(id));
                // `selfcheck` = 这个客户端答得上"启动前那一问"（见 `rpc_game_launch`）：
                // 认不出云端那一条时先别起游戏，把问题交回来（`SyncAskAnswered`）。
                params.insert("selfcheck".into(), Value::Bool(true));
                crate::rpc::call(&socket, "game.launch", Some(params)).await
            },
            Message::LaunchDone,
        )
    }

    /// 启动前那一问的回答：先把回答交给 daemon（`sync.resolve`），**再起一次**。
    ///
    /// 起两次是刻意的：这一次自检已经能定下来（回答刚写进配置），于是这一次才真的
    /// 拉存档、起游戏。
    pub(super) fn sync_ask_answered(&mut self, choice: String) -> Task<Message> {
        let Some(game_id) = self.sync_ask.take() else {
            return Task::none();
        };
        self.sync_ask_hidden = false;
        // 这一问结束了：云端那条"疑似找到的"事实也跟着作废（下次问会重新带一份）。
        self.sync_ask_cloud = None;
        self.launching = Some(game_id.clone());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move {
                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(game_id.clone()));
                params.insert("choice".into(), Value::String(choice));
                crate::rpc::call(&socket, "sync.resolve", Some(params)).await?;

                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(game_id));
                params.insert("selfcheck".into(), Value::Bool(true));
                crate::rpc::call(&socket, "game.launch", Some(params)).await
            },
            Message::LaunchDone,
        )
    }

    /// 启动那一问里挑定了云端的一条：交给 daemon（`sync.resolve { choice:"pair" }` 带上
    /// 身份），然后**再起一次** —— 这一次自检就能定下来，于是才真的拉存档、起游戏。
    ///
    /// 两个入口共用：弹窗里那颗「就绑这一条」（绑显示出来的那一条），以及「自己挑一条
    /// 绑上…」之后从云端清单里选中的那一条。
    pub(super) fn sync_ask_pair_with(
        &mut self,
        cloud_id: String,
        cloud_key: String,
    ) -> Task<Message> {
        let Some(game_id) = self.sync_ask.take() else {
            return Task::none();
        };
        self.sync_ask_hidden = false;
        // 这一问结束了：云端那条"疑似找到的"事实也跟着作废（下次问会重新带一份）。
        self.sync_ask_cloud = None;
        self.launching = Some(game_id.clone());
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move {
                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(game_id.clone()));
                params.insert("choice".into(), Value::String("pair".to_string()));
                params.insert("cloud_id".into(), Value::String(cloud_id));
                params.insert("cloud_key".into(), Value::String(cloud_key));
                crate::rpc::call(&socket, "sync.resolve", Some(params)).await?;

                let mut params = serde_json::Map::new();
                params.insert("id".into(), Value::String(game_id));
                params.insert("selfcheck".into(), Value::Bool(true));
                crate::rpc::call(&socket, "game.launch", Some(params)).await
            },
            Message::LaunchDone,
        )
    }

    /// 弹窗里那颗「就绑这一条」：绑上弹窗里显示的那一条（"疑似找到"时才有这颗按钮）。
    pub(super) fn sync_ask_bind_found(&mut self) -> Task<Message> {
        let Some(cloud) = self.sync_ask_cloud.clone() else {
            return Task::none();
        };
        self.sync_ask_pair_with(cloud.cloud_id, cloud.cloud_key)
    }

    /// 启动前那一问被关掉（点空白 / 关闭）：等于「关掉这一款的同步」，然后**照常启动**。
    ///
    /// 用户 2026-09-24 定的：关掉浮层不该变成"这一次不起游戏"，关掉这一款的同步就够 ——
    /// 而且这一款以后还能在单游戏页里自己打开（那颗开关就是为这条出路做的）。
    pub(super) fn sync_ask_declined(&mut self) -> Task<Message> {
        self.sync_ask_answered("off".to_string())
    }

    /// 「改配对…」：收起那一问、打开云端清单（`sync_ask` 留着 —— 挑完要用它启动）。
    ///
    /// 清单读的是**本机缓存**；挑中与关闭分别落回 [`Self::sync_ask_paired`] 与
    /// [`Self::sync_ask_declined`]（见 `update/add.rs` 的 `CloudPickChoose` /
    /// `CloudPickDismiss`）。
    pub(super) fn sync_ask_pair_requested(&mut self) -> Task<Message> {
        self.sync_ask_hidden = true;
        self.cloud_pick.open(CloudPickPurpose::Launch);
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { cloud_list(&socket, false).await },
            Message::CloudPickLoaded,
        )
    }

    /// 换绑 / 新建这一款的云端身份 —— 单游戏页那两个入口共用（都**不**启动游戏）。
    ///
    /// `cloud` 给 `None` 就是新建：daemon 会清掉本机记的身份，下次上传时新建一条（云端
    /// 旧的那条不会删，随时能再绑回来）。换绑与新建都是明确的用户动作，界面上都先问过。
    pub(super) fn change_binding(&mut self, cloud: Option<(String, String)>) -> Task<Message> {
        let Some(id) = self.selected.clone() else {
            return Task::none();
        };
        self.saved_ok = true;
        self.saved_msg = Some(if cloud.is_some() {
            "正在换绑…".to_string()
        } else {
            "正在新建云端身份…".to_string()
        });
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { resolve_pairing(&socket, id, cloud).await },
            Message::SyncBindingChanged,
        )
    }

    /// 停掉这一款正在跑的会话。会话表里没有它就什么都不做(界面本该是灰的)。
    fn stop_game(&mut self, game_id: String) -> Task<Message> {
        let Some(session) = self.running.get(&game_id).map(|s| s.session_id.clone()) else {
            return Task::none();
        };
        self.confirm_stop = false;
        let socket = self.daemon_socket.clone();
        self.error = None;
        Task::perform(
            async move { stop_session(&socket, &session).await },
            Message::StopDone,
        )
    }
}

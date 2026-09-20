//! 缩放做不了的那一端：**启动、观测与同步照常，缩放归外部工具。**
//!
//! 先说清楚它不是什么：**它不是"还没写完"。** 在 Windows 上缩放本来就不归
//! kotori 管 —— 它是外部工具（Magpie）的事，而那个工具留给第三方的接口
//! **只能观察、不能下命令**（广播消息 + 窗口属性，见 `PLATFORMS.md` §2.3）。
//! 所以"kotori 自己把画面缩起来"这件事在 Windows 上根本没有对应的动作。
//!
//! 但"启动游戏"与"跟着游戏、退出后上传存档"是 kotori 的份内事，而且三端的
//! 语义对称（`PLATFORMS.md` §2.2）。所以这个后端管理真实的会话：
//!
//! * `start_session`（用户点「启动」）= **直接启动**：这台机器没有 gamescope
//!   可套，点击启动就是直接把 exe 跑起来（用户 2026-09-19：不再报错）；
//! * `spec.watch_only`（**观测会话**）= 只盯进程，与 Linux 同一条路径。它由
//!   `daemon::watch` 发起（进程已经在跑），不是一种"启动方式"；
//! * `Ended` 事件照发 —— 退出后的存档上传因此在 Windows 上工作。
//!
//! 缩放动作（`scale.action`）仍然全部回 [`ScaleError::Unsupported`]：那是
//! "这件事在这里不归我们"，能力表据此把缩放编辑禁掉，界面不撒谎。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use super::{LaunchSpec, ScaleEngine, ScaleError, ScaleSession, ScaleStatus, SessionEvent, direct};

/// Windows 上的后端：会话是真的，缩放没有。
#[derive(Debug)]
pub struct UnsupportedScaleEngine {
    sessions: Arc<RwLock<HashMap<String, ScaleSession>>>,
    events: broadcast::Sender<SessionEvent>,
}

impl UnsupportedScaleEngine {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            events: broadcast::channel(64).0,
        }
    }
}

#[async_trait::async_trait]
impl ScaleEngine for UnsupportedScaleEngine {
    /// 用户点「启动」= 直接启动（`watch_only` 的游戏则是纯观测）。
    async fn start_session(&self, spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
        if spec.watch_only {
            return direct::start_watch_session(&self.sessions, &self.events, spec).await;
        }
        direct::start_direct_session(&self.sessions, &self.events, spec).await
    }

    /// 直接启动的会话没有进程组可一锅端（Windows 没有 `process_group`）：
    /// "停止" = kotori 不再跟踪这一局，游戏本身继续跑。
    async fn stop_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        let removed = self.sessions.write().await.remove(&session.session_id);
        match removed {
            Some(_) => Ok(()),
            None => Err(ScaleError::SessionNotFound(session.session_id.clone())),
        }
    }

    async fn wait_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        loop {
            if self.get_session(&session.session_id).await.is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        Ok(())
    }

    async fn get_session(&self, session_id: &str) -> Option<ScaleSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    async fn list_sessions(&self) -> Vec<ScaleSession> {
        self.sessions.read().await.values().cloned().collect()
    }

    fn subscribe(&self) -> Option<broadcast::Receiver<SessionEvent>> {
        Some(self.events.subscribe())
    }

    async fn get_status(&self, _session: &ScaleSession) -> Result<ScaleStatus, ScaleError> {
        Err(ScaleError::Unsupported)
    }
}

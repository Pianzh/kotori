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

    /// 「停止」= **真的把这一局结束掉**(我们启动的那个进程树),或者**不再跟着它**
    /// (观测会话)。
    ///
    /// ⚠ 从前这里只把会话从表里删掉 —— 于是 Windows 上那颗「停止」按钮的实际效果是
    /// "kotori 不再跟着这一局,游戏照跑"(用户 2026-09-20 实测:"只会停止对游戏进程的
    /// 追逐而不会停止进程")。现在两头都对上了 Linux:`terminate_session` 干的事这里
    /// 由 [`crate::process::kill_tree`] 干(Windows 没有进程组,只有"走一遍后代")。
    async fn stop_session(&self, session: &ScaleSession) -> Result<(), ScaleError> {
        if !self.sessions.read().await.contains_key(&session.session_id) {
            return Err(ScaleError::SessionNotFound(session.session_id.clone()));
        }

        // 观测会话里的进程不是我们启动的:对它动手就是越界。删掉会话 = 停止跟随。
        if session.watch_only {
            self.sessions.write().await.remove(&session.session_id);
            tracing::info!("stopped watch-only session {}", session.session_id);
            return Ok(());
        }

        // `gamescope_pid` 在类型上是 u32(它是给别处显示用的句柄/进程号),而进程表
        // 一律用 i32 —— 转一道,顺手把"没有 pid"这种不可能的情况挡住。
        let pid = session.gamescope_pid.unwrap_or(0) as i32;
        let mut killed = crate::process::kill_tree(pid);
        // 启动器交接那种游戏:我们 spawn 的那个早就退了(会话是照着进程名继续跟的),
        // 所以按 pid 杀不到任何东西 —— 那就把顶着这个名字的进程各杀一棵树。Linux 侧
        // 不需要这一步:交接出去的孩子仍然留在同一个进程组里,`killpg` 顺带扫到它。
        if killed == 0
            && let Some(name) = session.process_name.as_deref()
        {
            for other in crate::process::find_pids(name) {
                killed += crate::process::kill_tree(other);
            }
        }
        tracing::info!(
            "stopping session {} (pid {pid}, killed {killed})",
            session.session_id
        );

        // 会话**留在表里**:进程走光由 `direct::spawn_watch_task` 发现,它照旧发
        // `Ended` —— 而 `Ended` 是"退出后上传存档"的唯一触发器。Linux 侧的
        // `terminate_session` 之后同样什么都不做,两边因此一模一样。
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScaleProfile;

    /// 「停止」必须**真的把这一局结束掉**(我们启动的那一局)。
    ///
    /// 用户 2026-09-20 在 Windows 上实测报的就是这个:从前 `stop_session` 只把会话从
    /// 表里删掉,于是那颗按钮的实际效果是"kotori 不再跟着它,游戏照跑"。现在它走的
    /// 是 [`crate::process::kill_tree`],所以要钉住两件事:**整棵树**都得死,而会话
    /// 要留在表里(退出后的存档上传挂在 watcher 发出的 `Ended` 上)。
    ///
    /// 只能在 Windows 上跑(这个后端本身也只存在于 Windows);`cmd /c ping` 是"会一直
    /// 跑、而且有孩子"的现成样本 —— 拿 `notepad` 那种 GUI 进程在无头会话里不一定起得来。
    #[tokio::test]
    async fn stopping_a_launched_game_kills_its_process_tree() {
        let engine = UnsupportedScaleEngine::new();
        let dir = std::env::temp_dir();
        let profile = ScaleProfile::default_for();
        let cmd = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let args: Vec<String> = ["/c", "ping", "-n", "120", "127.0.0.1"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();

        let spec = LaunchSpec {
            game_id: "stop-me",
            exe: &cmd,
            args: &args,
            game_dir: &dir,
            wine_prefix: None,
            profile: &profile,
            process_name: None,
            watch_only: false,
            direct_launch: true,
        };
        let session = engine.start_session(&spec).await.expect("直启该成功");
        let root = session.gamescope_pid.expect("直启必须有 pid") as i32;

        // 让 cmd 把 ping 起出来(这一步要等,`spawn` 返回时孩子还没影)。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let descendants_of_root = loop {
            let found = crate::process::descendants(root);
            if !found.is_empty() {
                break found;
            }
            assert!(std::time::Instant::now() < deadline, "cmd 没有起出 ping");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        engine.stop_session(&session).await.expect("停止该成功");

        // 会话还在(收尾由 watcher 发现进程走光之后做,`Ended` 才发得出去)。
        assert!(
            engine.get_session(&session.session_id).await.is_some(),
            "停止之后会话要留着,否则退出后的上传就没触发器了"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let snapshot = crate::process::Snapshot::take();
            let left: Vec<i32> = std::iter::once(root)
                .chain(descendants_of_root.iter().copied())
                .filter(|pid| snapshot.has_pid(*pid))
                .collect();
            if left.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "停止之后这些进程还活着: {left:?} —— 那颗按钮又变成「只是不再跟踪」了"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

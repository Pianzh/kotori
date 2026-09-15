//! 与守护进程的连接:自动重连的次数上限、轮询间隔,以及 `daemon.status` 报回来的
//! 一条活动会话。
//!
//! 只有这几个数字和那个小结构体 —— 它们回答「连没连上、连上之后谁在跑」,和游戏 /
//! 云同步那些状态没有关系,也不需要从 `super` 取任何东西。

/// Maximum number of automatic reconnect attempts before giving up (a manual
/// "重连" always works, and resets the counter).
pub(in crate::ui) const MAX_AUTO_RETRIES: u32 = 5;

/// How often the UI polls the daemon for live sessions.
pub(in crate::ui) const STATUS_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// A live session, as reported by `daemon.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub watch_only: bool,
}

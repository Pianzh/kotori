//! 没有缩放可做的那一端：**让 trait 有个实现，而不是让编译停下来**。
//!
//! 先说清楚它不是什么：**它不是"还没写完"。** 在 Windows 上缩放本来就不归
//! kotori 管 —— 它是外部工具（Magpie）的事，而那个工具留给第三方的接口
//! **只能观察、不能下命令**（广播消息 + 窗口属性，见 `PLATFORMS.md` §2.3）。
//! 所以"kotori 自己把画面缩起来"这件事在 Windows 上根本没有对应的动作。
//!
//! 它存在的意义只有一个：**让上层保持一套形状**。`ScaleEngine` 是 daemon 持有
//! 的东西，`daemon` 和 `scale_rpc` 都按这个 trait 写；如果 Windows 上干脆没有
//! 实现，那这些代码就得整个 `#[cfg]` 掉，也就等于给 Windows 写第二套 daemon。
//!
//! ⚠ **界面的样子还没定。** Linux 那边"缩放"是 kotori 自己的状态（能加能减、
//! 有阶梯、有 sharpness），Windows 这边将来顶多是"某个外部工具正在缩放这个游戏"
//! 的一个观察结果（[`crate::platform`] 的能力表里已经按"外部工具提供 · 可观察 ·
//! 不可控"记着了）。两者形状不同，所以**界面迟早要分开**，但那是以后的事：
//! 现在所有动作都回 [`ScaleError::Unsupported`]，能力表据此把缩放相关的编辑禁掉，
//! 界面至少不会撒谎。

use super::{LaunchSpec, ScaleEngine, ScaleError, ScaleSession, ScaleStatus};

/// Windows 上的缩放后端：每个动作都说"这里没有这回事"。
#[derive(Debug, Default)]
pub struct UnsupportedScaleEngine;

impl UnsupportedScaleEngine {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ScaleEngine for UnsupportedScaleEngine {
    /// 不启动任何东西。
    ///
    /// ⚠ 注意这一条**以后会变**：`LaunchSpec::watch_only` 的注释里写着"用户在
    /// Windows 上自己启动游戏才是常态"，而"启动前取回存档 / 退出后上传"这条
    /// 会话语义对三端是对称的（`PLATFORMS.md` §2.2）—— 那需要的是**监视**进程，
    /// 不是缩放。等做到那一步时，这里会变成"只监视、不缩放"，而不是继续报错。
    async fn start_session(&self, _spec: &LaunchSpec<'_>) -> Result<ScaleSession, ScaleError> {
        Err(ScaleError::Unsupported)
    }

    /// 没有会话可停 —— 因为没有会话。
    ///
    /// 这里回 `Ok` 而不是 `Err` 是刻意的：启动已经在上一步失败了，收尾时再报一次
    /// 错只是噪音。真正"没有这件事"的判断留给 [`ScaleError::Unsupported`] 出现的那一处。
    async fn stop_session(&self, _session: &ScaleSession) -> Result<(), ScaleError> {
        Ok(())
    }

    async fn wait_session(&self, _session: &ScaleSession) -> Result<(), ScaleError> {
        Err(ScaleError::Unsupported)
    }

    async fn get_session(&self, _session_id: &str) -> Option<ScaleSession> {
        None
    }

    async fn list_sessions(&self) -> Vec<ScaleSession> {
        Vec::new()
    }

    // `subscribe` 刻意不实现：trait 的默认实现就回 `None`，而它的文档正好写着
    // "一个报不出事件的 backend 就是没有同步触发点，这是受支持的状态，不是错误"。
    // 这正是 Windows 现在的样子。

    async fn get_status(&self, _session: &ScaleSession) -> Result<ScaleStatus, ScaleError> {
        Err(ScaleError::Unsupported)
    }
}

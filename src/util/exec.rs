//! 起子进程时那件**两个平台不一样、但每个调用点都不该操心**的事:别弹控制台窗口。
//!
//! Windows 上,从一个 GUI 子系统进程(`windows_subsystem = "windows"`,见 `main.rs`)
//! 里 spawn 一个控制台程序,系统默认会**给它新建一个控制台** —— 用户看到的就是一个
//! 黑框闪一下。而 kotori 起子进程的地方偏偏不少:设置页的「环境检查」要连跑好几个
//! `--version`(kopia / rclone / 文件对话框 / 分辨率),每次同步要起 kopia,换引擎要
//! 起 rclone。于是"切到设置页"在真机上表现为**一串黑框**(用户 2026-09-18 报的)。
//!
//! 还有一层:控制台窗口不只是难看。`ensure_running` 起的守护进程如果带控制台,那个
//! 窗口会一直存在到守护进程退出,而**用户关掉它等于给守护进程发 Ctrl-C**。
//!
//! Unix 上不存在这件事,所以下面两个实现里一个是真的、一个是空操作 —— 调用点因此
//! 一行 `cfg` 都不用写。

use std::process::Command as StdCommand;
use tokio::process::Command as TokioCommand;

/// `CreateProcess` 的 `CREATE_NO_WINDOW`。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 「这个子进程不许有自己的窗口」。
///
/// 两种命令各实现一次(`std` 与 `tokio` 是两种类型),调用点因此不必管自己手上是哪一个。
pub(crate) trait Quiet {
    fn quiet(&mut self) -> &mut Self;
}

impl Quiet for StdCommand {
    fn quiet(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

impl Quiet for TokioCommand {
    fn quiet(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            // tokio 的这条路会自己 OR 上 `CREATE_UNICODE_ENVIRONMENT`(见它的文档),
            // 所以这里只管给窗口那一位。
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「不弹窗」在 Unix 上是空操作,但它**不许改坏别的设置** —— 这两条测试跑在
    /// Linux 上,验的就是"没副作用";Windows 那一半由 CI 的 `cargo check` 保证编得过,
    /// 真实效果在 Windows VM 上看(见 `PLATFORMS.md`)。
    #[test]
    fn quiet_leaves_a_std_command_usable() {
        let mut command = StdCommand::new("echo");
        command.arg("hi").env("KOTORI_TEST", "1");
        command.quiet();
        assert_eq!(command.get_args().count(), 1);
        assert!(command.get_envs().any(|(k, _)| k == "KOTORI_TEST"));
    }

    #[test]
    fn quiet_leaves_a_tokio_command_usable() {
        let mut command = TokioCommand::new("echo");
        command.arg("hi");
        command.quiet();
        assert_eq!(command.as_std().get_args().count(), 1);
    }
}

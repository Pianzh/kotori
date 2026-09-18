//! 图形后端的运行时回退。
//!
//! Slint 的渲染器是**编译期**就定下来的(`renderer-femtovg` 之类的 feature),运行时只有
//! 「要求用某一个」这一种表达方式(`BackendSelector::renderer_name`),而且
//! `i_slint_core::platform::set_platform` **只允许成功一次** —— 所以「先试 OpenGL,
//! 不行再换软件渲染」没法在同一个进程里做第二遍。
//!
//! 没有 GPU 的机器上会撞上这个(2026-09-18 在 Windows VM 上实测):
//!
//! ```text
//! Error: Failed to initialize OpenGL driver: Could not locate glCreateShader symbol
//! ```
//!
//! femtovg 要 OpenGL 2.0+,而 Windows 自带的 `opengl32.dll` 只到 1.1 —— `glCreateShader`
//! 是 2.0 才有的符号。虚拟机、远程桌面、只装了「Microsoft 基本显示适配器」的机器都会中。
//! Slint 自己**不会**回退:`i-slint-backend-winit` 的 `create_renderer` 在
//! `renderer_name` 为 `None` 时直接走 `default_renderer_factory`(编译期的 femtovg),
//! 那个 `allow_fallback` 只处理「名字不认识」,不处理「初始化失败」。
//!
//! 办法:**带着 `SLINT_BACKEND=winit-software` 把自己重启一次**。用重启而不是重试,
//! 就是因为上面的 `set_platform` 只能成功一次。再用一个环境变量记住「已经退过一次」,
//! 免得软件渲染也起不来时无限重启。

use std::process::Command;

/// 已经因为图形后端退过一次了。
const RETRIED: &str = "KOTORI_RENDERER_RETRIED";

/// 交给 Slint 自己的环境变量。
const BACKEND: &str = "SLINT_BACKEND";

/// 软件渲染的写法,见 `i-slint-backend-winit` 的 `create_renderer`:
/// `(Some("sw"), None) | (Some("software"), None)` —— 但 `SLINT_BACKEND` 走的是
/// `parse_backend_env_var`,名字要带后端前缀。
const SOFTWARE: &str = "winit-software";

/// 这个失败值得换软件渲染再试一次吗?
///
/// 判据故意放宽:别的图形栈上 Slint 还有别的说法(EGL、D3D、bind API),而「多退一次
/// 软件渲染」的代价很小,漏判的代价却是用户看到一句看不懂的报错、程序直接退出。
fn is_graphics_failure(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    [
        "OpenGL",
        "glCreateShader",
        "graphics API",
        "Failed to initialize",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// 该不该退?用户自己指定过后端就**不退** —— 他既然写明了,报错比背着他换掉诚实。
fn should_fall_back(error: &anyhow::Error) -> bool {
    if std::env::var_os(RETRIED).is_some() {
        return false;
    }
    if std::env::var_os(BACKEND).is_some() {
        return false;
    }
    is_graphics_failure(error)
}

/// 带着软件渲染重启自己,返回 `Ok(())` —— 调用方到此正常退出就行。
fn relaunch_with_software() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    tracing::warn!(
        "图形后端起不来,换软件渲染重开一次(原进程退出):{}",
        exe.display()
    );

    // 不带参数重启:UI 本来就是不给子命令时的默认动作。
    // 父进程退出不会带走守护进程 —— 它是独立拉起来的,不在同一个作业对象里。
    Command::new(&exe)
        .env(BACKEND, SOFTWARE)
        .env(RETRIED, "1")
        .spawn()?;

    Ok(())
}

/// `driver::run()` 的收尾:图形后端失败就退一次软件渲染,其余错误照实往上抛。
pub(super) fn finish(result: anyhow::Result<()>) -> anyhow::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if should_fall_back(&e) => relaunch_with_software(),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VM 上实际报出来的那一句。
    #[test]
    fn the_opengl_failure_we_actually_hit_is_recognised() {
        let error = anyhow::anyhow!(
            "Failed to initialize OpenGL driver: Could not locate glCreateShader symbol"
        );
        assert!(is_graphics_failure(&error));
    }

    /// 不认识的失败不该被当成图形问题 —— 那会把真正的错误吞掉。
    #[test]
    fn an_unrelated_failure_is_not_a_graphics_failure() {
        let error = anyhow::anyhow!("配置文件读不出来");
        assert!(!is_graphics_failure(&error));
    }
}

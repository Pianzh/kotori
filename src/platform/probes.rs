//! 各项探测：一条 `--version`、一个系统调用，各自回答"这台机器行不行"。
//!
//! 与 `mod.rs` 分开，是因为那边只关心**报告长什么样**（`Check` / `Report` 与
//! 序列化形状），这里只关心**怎么问**。发行版相关的"怎么装"在 `distro`。
//!
//! 三种结局必须分清（[`Ran`]）：**超时说明装着**，只是没在预算内回答；只有真的
//! 起不来才是"没装"。把两者混为一谈会让用户去装一个本来就有的东西。

use std::process::Stdio;

use super::distro::{Distro, Package};
use super::{Check, Level, MIN_GAMESCOPE, PROBE_TIMEOUT, State};

pub(super) async fn gamescope(distro: &Distro) -> Check {
    const IMPACT: &str = "没有它就启动不了游戏:kotori 是用 gamescope 把 wine 拉起来的,\
                          缩放增强(FSR / 整数缩放 / 锐度)也全靠它";
    let install = distro.install(Package::same("gamescope"));
    match run("gamescope", &["--version"]).await {
        Ran::Out(text) => {
            let detail = first_line(&text, "已安装");
            match gamescope_version(&detail) {
                Some(version) if version < MIN_GAMESCOPE => Check::degraded(
                    "gamescope",
                    "gamescope",
                    Level::Required,
                    // 把原始那行也留着:用户要拿它去搜/去问。
                    format!(
                        "{detail} —— 比 kotori 要求的 {}.{} 旧",
                        MIN_GAMESCOPE.0, MIN_GAMESCOPE.1
                    ),
                    "旧版没有 `-S`/`-F`/`--sharpness` 这套参数,缩放不会按你设的那样生效;升级它",
                    install,
                ),
                _ => Check::ready("gamescope", "gamescope", detail, "启动游戏与缩放增强"),
            }
        }
        // 装着、但问不出话:别下"没装"的结论,也别假装它好。
        Ran::Timeout => Check::degraded(
            "gamescope",
            "gamescope",
            Level::Required,
            "装了,但 `gamescope --version` 没有在 5 秒内回答".to_string(),
            "启动游戏时可能会卡住,值得先手动跑一次看看",
            String::new(),
        ),
        Ran::Failed => Check::missing("gamescope", "gamescope", IMPACT, install),
    }
}

pub(super) async fn wine(distro: &Distro) -> Check {
    const IMPACT: &str = "没有它就启动不了 Windows 游戏;已经配好的 wine prefix 也需要它来跑";
    let install = distro.install(Package::same("wine"));
    match run("wine", &["--version"]).await {
        Ran::Out(text) => Check::ready(
            "wine",
            "wine",
            first_line(&text, "已安装"),
            "启动 Windows 游戏、管理 prefix",
        ),
        Ran::Timeout => Check::degraded(
            "wine",
            "wine",
            Level::Required,
            "装了,但 `wine --version` 没有在 5 秒内回答".to_string(),
            "启动游戏时可能会卡住,值得先手动跑一次看看",
            String::new(),
        ),
        Ran::Failed => Check::missing("wine", "wine", IMPACT, install),
    }
}

pub(super) async fn rclone(distro: &Distro, configured: &str) -> Check {
    const IMPACT: &str = "没有它就用不了 rclone 那种同步方式(默认就是它);选了 kopia 的机器可以不要";
    let install = distro.install(Package::same("rclone"));
    // 设置页里可以指点位置，之后才是 `KOTORI_RCLONE` 与 PATH（见 `sync::executables`）。
    let Some(binary) = crate::sync::find_rclone(configured) else {
        return Check::missing("rclone", "rclone", IMPACT, install);
    };
    let path = binary.display().to_string();
    match run(&path, &["version"]).await {
        Ran::Out(text) => Check::ready(
            "rclone",
            "rclone",
            format!("{}（{path}）", first_line(&text, "已安装")),
            "云存档同步",
        ),
        Ran::Timeout => Check::degraded(
            "rclone",
            "rclone",
            Level::Required,
            format!("找到了 {path},但 `rclone version` 没有在 5 秒内回答"),
            "同步可能会一直卡着,值得先手动跑一次看看",
            String::new(),
        ),
        Ran::Failed => Check::missing("rclone", "rclone", IMPACT, install),
    }
}

/// kopia 那**一种**同步方式（rclone 是另一种，两者互不依赖）。
///
/// ⚠ Arch 官方仓库**没有** kopia，只有 archlinuxcn 有，所以安装命令里带着仓库名。
pub(super) async fn kopia(distro: &Distro, configured: &str) -> Check {
    const IMPACT: &str =
        "没有它就用不了 kopia 那种同步方式(增量、去重、自带加密);rclone 那条路不受影响";
    let install = distro.install(Package::per_distro("archlinuxcn/kopia", "kopia", "kopia"));
    let Some(binary) = crate::sync::find_kopia(configured) else {
        // **可选**:没有它 rclone 那条路照常,报告不该因此判成"这台机器不行"。
        return Check::missing_optional("kopia", "kopia", IMPACT, install);
    };
    let path = binary.display().to_string();
    match run(&path, &["--version"]).await {
        Ran::Out(text) => Check::ready_optional(
            "kopia",
            "kopia",
            format!("{}（{path}）", first_line(&text, "已安装")),
            "kopia 同步方式(增量、去重、加密)",
        ),
        Ran::Timeout => Check::degraded(
            "kopia",
            "kopia",
            Level::Optional,
            format!("找到了 {path},但 `kopia --version` 没有在 5 秒内回答"),
            "同步可能会一直卡着,值得先手动跑一次看看",
            String::new(),
        ),
        // 装没装是两件事:这里只在"起不来"时下"没装"的结论。
        Ran::Failed => Check::missing_optional("kopia", "kopia", IMPACT, install),
    }
}

pub(super) async fn file_dialog(distro: &Distro) -> Check {
    let install = distro.install(Package::same("xdg-desktop-portal"));
    match crate::picker::probe().await {
        Ok(()) => Check {
            id: "file-dialog",
            title: "系统文件对话框".to_string(),
            level: Level::Optional,
            state: State::Ready,
            detail: "桌面门户可用,各处「浏览…」都能弹出对话框".to_string(),
            impact: "挑游戏目录 / exe / wine prefix / 存档位置".to_string(),
            install: String::new(),
        },
        Err(reason) => Check {
            id: "file-dialog",
            title: "系统文件对话框".to_string(),
            level: Level::Optional,
            state: State::Degraded,
            detail: reason,
            impact: "「浏览…」按钮会灰掉,路径只能手打 —— 功能本身不受影响".to_string(),
            install,
        },
    }
}

pub(super) fn window_control() -> Check {
    let degraded = |detail: &str, impact: &str| {
        Check::degraded(
            "window-control",
            "窗口尺寸控制",
            Level::Optional,
            detail.to_string(),
            impact,
            String::new(),
        )
    };
    if crate::desktop::is_kde() {
        Check {
            id: "window-control",
            title: "窗口尺寸控制".to_string(),
            level: Level::Optional,
            state: State::Ready,
            detail: "KDE Plasma:运行时改变游戏窗口尺寸可用".to_string(),
            impact: "按比例放大 / 缩小正在玩的游戏窗口".to_string(),
            install: String::new(),
        }
    } else {
        degraded(
            "只在 KDE Plasma 上实现(平铺桌面里窗口尺寸是布局的事)",
            "改窗口尺寸会如实回「做不到」;滤镜与锐度仍然可用",
        )
    }
}

pub(super) fn resolution() -> Check {
    match crate::display::primary_resolution() {
        Some((width, height)) => Check {
            id: "resolution",
            title: "输出分辨率探测".to_string(),
            level: Level::Optional,
            state: State::Ready,
            detail: format!("探测到 {width}×{height}"),
            impact: "窗口尺寸留空时按屏幕开窗".to_string(),
            install: String::new(),
        },
        None => Check::degraded(
            "resolution",
            "输出分辨率探测",
            Level::Optional,
            "没探测到(桌面不认识 / 工具不在)".to_string(),
            "留空时按内置默认开窗;也可以设 KOTORI_OUTPUT_RESOLUTION=宽x高",
            String::new(),
        ),
    }
}

/// 系统密钥环。没有它**不是错误**:默认就是 0600 的明文凭据文件(ADR-014)。
pub(super) fn keyring(distro: &Distro) -> Check {
    let install = distro.install(Package::per_distro(
        "libsecret",
        "libsecret-tools",
        "libsecret",
    ));
    // ⚠ `system()` 只探测、不搬家,可以放心在"看一眼"的路径上调用。
    match crate::secrets::Keyring::system() {
        Ok(_) => Check {
            id: "keyring",
            title: "系统密钥环".to_string(),
            level: Level::Optional,
            state: State::Ready,
            detail: "可用:凭据可以交给桌面的密钥环保管".to_string(),
            impact: "零输入地保管 B2 凭据".to_string(),
            install: String::new(),
        },
        Err(error) => Check {
            id: "keyring",
            title: "系统密钥环".to_string(),
            level: Level::Optional,
            state: State::Degraded,
            detail: error.to_string(),
            impact: "凭据改用权限 0600 的明文文件,或者你自己设的主密码文件 —— 两者都是正常状态"
                .to_string(),
            install,
        },
    }
}

/// 一条外部命令的三种结局。区分「超时」和「失败」很重要:前者说明**装着**。
pub(super) enum Ran {
    Out(String),
    Timeout,
    Failed,
}

pub(super) async fn run(binary: &str, args: &[&str]) -> Ran {
    let mut command = tokio::process::Command::new(binary);
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Err(_) => Ran::Timeout,
        Ok(Err(_)) => Ran::Failed,
        Ok(Ok(output)) => {
            // 版本信息有的程序写 stdout,有的写 stderr —— 两边都收,再让退出码说话。
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.trim().is_empty() {
                text = String::from_utf8_lossy(&output.stderr).to_string();
            }
            if output.status.success() || !text.trim().is_empty() {
                Ran::Out(text)
            } else {
                Ran::Failed
            }
        }
    }
}

/// 版本输出取第一行、剥掉颜色转义与日志前缀、限长 —— 它会直接进 GUI 的一行文字里。
pub(super) fn first_line(text: &str, fallback: &str) -> String {
    let plain = strip_ansi(text);
    let line = plain
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fallback);
    strip_log_prefix(line).chars().take(120).collect()
}

/// 去掉 `[gamescope] [Info]  console: ` 这类日志装饰。
///
/// 不剥的话设置页会显示成「[gamescope] [Info]  console: gamescope version 3.16.28」——
/// 用户要看的是版本号,前面那串是他程序自己的日志格式。
fn strip_log_prefix(line: &str) -> String {
    let mut rest = line.trim_start();
    // 反复吃掉开头的 `[…]` 组。
    while rest.starts_with('[')
        && let Some(end) = rest.find(']')
    {
        rest = rest[end + 1..].trim_start();
    }
    // 再吃掉一个 `console:` 这样的小写标签。
    if let Some((label, tail)) = rest.split_once(':')
        && !label.is_empty()
        && label.len() <= 12
        && label.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')
    {
        rest = tail.trim_start();
    }
    rest.to_string()
}

/// 从版本输出里认出 `主.次`。认不出来就返回 `None`(那就别下"太旧"的结论)。
pub(super) fn gamescope_version(text: &str) -> Option<(u32, u32)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let major_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'.' {
            continue;
        }
        let major = text[major_start..index].parse().ok()?;
        index += 1;
        let minor_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if minor_start == index {
            continue;
        }
        let minor = text[minor_start..index].parse().ok()?;
        return Some((major, minor));
    }
    None
}

/// 去掉 ANSI 颜色转义(`gamescope --version` 会带一串)。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // `ESC [ … <字母>`:一直吃到那个收尾字母为止。
        if chars.next() == Some('[') {
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

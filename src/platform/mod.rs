//! 「这台机器上到底有什么、缺了会怎样」—— 依赖探测。
//!
//! 形状是用户 2026-09-13 定的:**环境检查只出现在 GUI 的设置页**,不做 CLI doctor。
//! 这一份数据将来还要当「界面不许撒谎」的依据(Windows 上缩放这些功能不存在),
//! 所以它回答的不是"装没装",而是**"现在能不能用、不能用时退化成什么"**。
//!
//! ⚠ 为什么每一项都尽量**真做一次最小动作**(`--version`、真建一次 portal 代理、
//! 拿密钥环问一次它答得上来的问题),而不是 `which` 一下:我们已经被"装了 ≠ 能用"
//! 咬过三次 —— 密钥环装了但桌面没把它拉起来、portal 的名字在总线上但它是按需激活的、
//! `secret-tool store` 没读 stdin 就退出。
//!
//! ⚠ 探测**不许改状态**:这里绝不能调 `Keyring::open_default` —— 它会把明文凭据搬进
//! 密钥环(ADR-014 的挑选顺序),"点一下环境检查"不该产生这种副作用。这里只用
//! [`crate::secrets::Keyring::system`],它只问不搬。

use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

/// 单条命令的上限。探测是用户点开设置页时做的,不能让它拖着页面。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// kotori 的启动参数模型针对 gamescope 3.16(`-S`/`-F`/`--sharpness` 是这一版之后的形状)。
const MIN_GAMESCOPE: (u32, u32) = (3, 16);

/// 这一项是"没有它就不行",还是"没有它也能用,只是退化成别的样子"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// 现在就能用。
    Ready,
    /// 能用,但有个条件不满足(只在 KDE 上、版本偏旧、探测超时……)。
    Degraded,
    /// 没有它。
    Missing,
}

/// 一行检查。字段名就是 wire 上的字段名(UI 在 `ui/parse.rs` 里读它们)。
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// 稳定 id:UI 与将来的能力表靠它认人,别改。
    pub id: &'static str,
    pub title: String,
    pub level: Level,
    pub state: State,
    /// 现在是什么情况(版本 / 路径 / 为什么退化),一行。
    pub detail: String,
    /// 少了它就没有什么,或者退化成什么 —— 给人看的,不是给机器看的。
    pub impact: String,
    /// 按发行版给的安装命令;空 = 没什么可装的(桌面自带的,或本来就没事)。
    pub install: String,
}

impl Check {
    /// 一行"可以了"的检查。
    fn ready(id: &'static str, title: &str, detail: String, impact: &str) -> Self {
        Self {
            id,
            title: title.to_string(),
            level: Level::Required,
            state: State::Ready,
            detail,
            impact: impact.to_string(),
            install: String::new(),
        }
    }

    /// 一行"能用,但有条件"的检查。
    fn degraded(
        id: &'static str,
        title: &str,
        level: Level,
        detail: String,
        impact: &str,
        install: String,
    ) -> Self {
        Self {
            id,
            title: title.to_string(),
            level,
            state: State::Degraded,
            detail,
            impact: impact.to_string(),
            install,
        }
    }

    /// 一行"没有它"的检查。
    fn missing(id: &'static str, title: &str, impact: &str, install: String) -> Self {
        Self {
            id,
            title: title.to_string(),
            level: Level::Required,
            state: State::Missing,
            detail: "没找到".to_string(),
            impact: impact.to_string(),
            install,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `linux` / `windows` / 其它。
    pub platform: &'static str,
    /// 发行版的名字(`/etc/os-release` 的 PRETTY_NAME),认不出来就是空。
    pub distro: String,
    /// **必需项**全都在才为真;可选退化不影响它。
    pub ok: bool,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn from_checks(checks: Vec<Check>) -> Self {
        let ok = !checks
            .iter()
            .any(|check| check.level == Level::Required && check.state == State::Missing);
        Self {
            platform: platform(),
            distro: Distro::detect().name,
            ok,
            checks,
        }
    }
}

pub const fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

/// 探测一遍。
///
/// 三个 `--version` 顺序跑:它们都在几十毫秒内回答(实测),并发换来的复杂度不值。
/// 真正可能慢的是 portal 那一步,它自带 5 秒上限。
pub async fn report() -> Report {
    let distro = Distro::detect();
    let checks = vec![
        gamescope(&distro).await,
        wine(&distro).await,
        rclone(&distro).await,
        file_dialog(&distro).await,
        window_control(),
        resolution(),
        keyring(&distro),
    ];
    Report::from_checks(checks)
}

async fn gamescope(distro: &Distro) -> Check {
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

async fn wine(distro: &Distro) -> Check {
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

async fn rclone(distro: &Distro) -> Check {
    const IMPACT: &str = "没有它就没有云存档同步(退出后自动上传、启动前自动取回都不工作)";
    let install = distro.install(Package::same("rclone"));
    // 尊重 `KOTORI_RCLONE`:用户可能把它放在别处(测试也用它指向假货)。
    let Some(binary) = crate::sync::find_rclone() else {
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

async fn file_dialog(distro: &Distro) -> Check {
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

fn window_control() -> Check {
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

fn resolution() -> Check {
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
fn keyring(distro: &Distro) -> Check {
    let install = distro.install(Package {
        arch: "libsecret",
        debian: "libsecret-tools",
        fedora: "libsecret",
    });
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
enum Ran {
    Out(String),
    Timeout,
    Failed,
}

async fn run(binary: &str, args: &[&str]) -> Ran {
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
fn first_line(text: &str, fallback: &str) -> String {
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
fn gamescope_version(text: &str) -> Option<(u32, u32)> {
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

/// 发行版:只为了给对一条安装命令。
///
/// ⚠ 认不出来时**不猜** —— 宁可什么都不给,也不要给一条跑不通的命令。
struct Distro {
    name: String,
    family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Arch,
    Debian,
    Fedora,
    Unknown,
}

/// 同一个东西在不同发行版里包名不同,所以按发行版各写一份。
#[derive(Debug, Clone, Copy)]
struct Package {
    arch: &'static str,
    debian: &'static str,
    fedora: &'static str,
}

impl Package {
    const fn same(name: &'static str) -> Self {
        Self {
            arch: name,
            debian: name,
            fedora: name,
        }
    }
}

impl Distro {
    fn detect() -> Self {
        if !cfg!(target_os = "linux") {
            return Self {
                name: String::new(),
                family: Family::Unknown,
            };
        }
        std::fs::read_to_string("/etc/os-release")
            .map(|text| Self::parse(&text))
            .unwrap_or(Self {
                name: String::new(),
                family: Family::Unknown,
            })
    }

    fn parse(text: &str) -> Self {
        let (mut id, mut like, mut pretty) = (String::new(), String::new(), String::new());
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            // os-release 的值可以带引号,单双都有(实测 Manjaro 用单引号)。
            let value = value.trim().trim_matches(['"', '\'']);
            match key.trim() {
                "ID" => id = value.to_ascii_lowercase(),
                "ID_LIKE" => like = value.to_ascii_lowercase(),
                "PRETTY_NAME" => pretty = value.to_string(),
                _ => {}
            }
        }
        let haystack = format!("{id} {like}");
        let family = if haystack.contains("arch") {
            Family::Arch
        } else if haystack.contains("debian") || haystack.contains("ubuntu") {
            Family::Debian
        } else if haystack.contains("fedora")
            || haystack.contains("rhel")
            || haystack.contains("centos")
        {
            Family::Fedora
        } else {
            Family::Unknown
        };
        Self {
            name: if pretty.is_empty() { id } else { pretty },
            family,
        }
    }

    /// 这个包在这台机器上怎么装。空字符串 = 给不出命令(认不出发行版)。
    fn install(&self, package: Package) -> String {
        let name = match self.family {
            Family::Arch => package.arch,
            Family::Debian => package.debian,
            Family::Fedora => package.fedora,
            Family::Unknown => return String::new(),
        };
        match self.family {
            Family::Arch => format!("sudo pacman -S {name}"),
            Family::Debian => format!("sudo apt install {name}"),
            Family::Fedora => format!("sudo dnf install {name}"),
            Family::Unknown => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(level: Level, state: State) -> Check {
        Check {
            id: "x",
            title: "x".to_string(),
            level,
            state,
            detail: String::new(),
            impact: String::new(),
            install: String::new(),
        }
    }

    #[test]
    fn a_missing_optional_dependency_does_not_make_the_report_fail() {
        // 没有系统文件对话框、平铺桌面上不能改窗口尺寸 —— 这些都是能用的机器。
        let report = Report::from_checks(vec![
            check(Level::Required, State::Ready),
            check(Level::Optional, State::Missing),
            check(Level::Optional, State::Degraded),
        ]);
        assert!(report.ok, "{report:?}");

        // 少一个必需项就不是了。
        let report = Report::from_checks(vec![
            check(Level::Required, State::Ready),
            check(Level::Required, State::Missing),
        ]);
        assert!(!report.ok);
    }

    #[test]
    fn a_required_dependency_that_answers_badly_is_not_reported_as_missing() {
        // 装着但问不出话,与"没装"是两件事:前者让用户去查,后者让用户去装。
        let report = Report::from_checks(vec![check(Level::Required, State::Degraded)]);
        assert!(report.ok);
    }

    #[test]
    fn the_install_command_follows_the_distro() {
        let arch = Distro::parse("ID=arch\nPRETTY_NAME=\"Arch Linux\"\n");
        assert_eq!(arch.name, "Arch Linux");
        assert_eq!(
            arch.install(Package::same("rclone")),
            "sudo pacman -S rclone"
        );

        let debian = Distro::parse("ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04\"\n");
        assert_eq!(
            debian.install(Package::same("rclone")),
            "sudo apt install rclone"
        );
        // 同一个东西在不同发行版里包名不同:secret-tool 在 Debian 系叫 libsecret-tools。
        assert_eq!(
            debian.install(Package {
                arch: "libsecret",
                debian: "libsecret-tools",
                fedora: "libsecret",
            }),
            "sudo apt install libsecret-tools"
        );

        let fedora = Distro::parse("ID=fedora\nPRETTY_NAME=\"Fedora 41\"\n");
        assert_eq!(
            fedora.install(Package::same("rclone")),
            "sudo dnf install rclone"
        );

        // 认不出来就**不给命令** —— 给一条跑不通的比不给更糟。
        let unknown = Distro::parse("ID=nixos\nPRETTY_NAME=\"NixOS\"\n");
        assert_eq!(unknown.install(Package::same("rclone")), "");
    }

    #[test]
    fn os_release_is_read_without_assuming_quotes_and_order() {
        let distro = Distro::parse(
            "# comment\nPRETTY_NAME='Manjaro Linux'\nID=manjaro\nID_LIKE=\"arch\"\nHOME_URL=x\n",
        );
        assert_eq!(distro.name, "Manjaro Linux");
        assert_eq!(distro.family, Family::Arch);
    }

    #[test]
    fn a_version_line_becomes_one_plain_line() {
        // `gamescope --version` 真的长这样:颜色码 + 它自己的日志前缀。
        let raw = "\u{1b}[0;34m[gamescope]\u{1b}[0m \u{1b}[0;37m[Info]\u{1b}[0m  console: gamescope version 3.16.28 (gcc)\n第二行\n";
        assert_eq!(
            first_line(raw, "已安装"),
            "gamescope version 3.16.28 (gcc)",
            "用户要的是版本号,不是它的日志格式"
        );
        // 没有装饰的行原样留着。
        assert_eq!(first_line("wine-10.9\n", "已安装"), "wine-10.9");
        assert_eq!(first_line("   \n\n", "已安装"), "已安装");
    }

    #[test]
    fn the_gamescope_version_is_read_out_of_a_noisy_line() {
        assert_eq!(
            gamescope_version("gamescope version 3.16.28 (gcc 16.2.1)"),
            Some((3, 16))
        );
        assert_eq!(gamescope_version("wine-10.9"), Some((10, 9)));
        assert_eq!(gamescope_version("gamescope version 4.0"), Some((4, 0)));
        // 认不出来时不下"太旧"的结论。
        assert_eq!(gamescope_version("gamescope (unknown)"), None);
        assert_eq!(gamescope_version(""), None);
    }

    #[test]
    fn an_older_gamescope_is_degraded_not_ready() {
        let old = gamescope_version("gamescope version 3.15.1");
        assert!(old.is_some_and(|version| version < MIN_GAMESCOPE));
        let new = gamescope_version("gamescope version 3.16.0");
        assert!(new.is_some_and(|version| version >= MIN_GAMESCOPE));
    }

    #[test]
    fn the_wire_shape_is_what_the_settings_page_reads() {
        // UI 在 `ui/parse.rs` 里按这些字段名读;改了这里就得改那里。
        let value = serde_json::to_value(Check {
            id: "rclone",
            title: "rclone".to_string(),
            level: Level::Required,
            state: State::Ready,
            detail: "rclone v1.75.1".to_string(),
            impact: "云存档同步".to_string(),
            install: String::new(),
        })
        .unwrap();
        assert_eq!(value["level"], "required");
        assert_eq!(value["state"], "ready");
        assert_eq!(value["install"], "");
        assert!(value.get("id").is_some() && value.get("impact").is_some());
    }
}

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
//!
//! 文件分工:本文件只管**报告长什么样**(类型、`Report` 与调度);`probes` 管怎么问
//! 每一项;`distro` 管"缺了该怎么装"。

use std::time::Duration;

use serde::Serialize;

use distro::Distro;

/// 单条命令的上限。探测是用户点开设置页时做的,不能让它拖着页面。
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// kotori 的启动参数模型针对 gamescope 3.16(`-S`/`-F`/`--sharpness` 是这一版之后的形状)。
#[cfg(unix)]
pub(super) const MIN_GAMESCOPE: (u32, u32) = (3, 16);

mod distro;
mod probes;

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
    /// 一行"可以了"的检查(必需项)。
    pub(super) fn ready(id: &'static str, title: &str, detail: String, impact: &str) -> Self {
        Self::ready_at(id, title, Level::Required, detail, impact)
    }

    /// 一行"可以了"的检查,但这一项是可选的(缺了也能用别的路)。
    pub(super) fn ready_optional(
        id: &'static str,
        title: &str,
        detail: String,
        impact: &str,
    ) -> Self {
        Self::ready_at(id, title, Level::Optional, detail, impact)
    }

    pub(super) fn ready_at(
        id: &'static str,
        title: &str,
        level: Level,
        detail: String,
        impact: &str,
    ) -> Self {
        Self {
            id,
            title: title.to_string(),
            level,
            state: State::Ready,
            detail,
            impact: impact.to_string(),
            install: String::new(),
        }
    }

    /// 一行"能用,但有条件"的检查。
    pub(super) fn degraded(
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

    /// 一行"没有它"的检查(必需项)。
    pub(super) fn missing(id: &'static str, title: &str, impact: &str, install: String) -> Self {
        Self::missing_at(id, title, Level::Required, impact, install)
    }

    /// 一行"没有它"的检查,但这一项是可选的。
    pub(super) fn missing_optional(
        id: &'static str,
        title: &str,
        impact: &str,
        install: String,
    ) -> Self {
        Self::missing_at(id, title, Level::Optional, impact, install)
    }

    pub(super) fn missing_at(
        id: &'static str,
        title: &str,
        level: Level,
        impact: &str,
        install: String,
    ) -> Self {
        Self {
            id,
            title: title.to_string(),
            level,
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
/// 几个 `--version` 顺序跑:它们都在几十毫秒内回答(实测),并发换来的复杂度不值。
/// 真正可能慢的是 portal 那一步,它自带 5 秒上限。
/// 探一遍这台机器。
///
/// 收 [`crate::config::SyncConfig`]，是因为两项探测要尊重**设置页里填的程序位置**
/// （rclone / kopia 的目录或完整路径）—— 否则用户在界面上指了路，环境检查却还报
/// "没装"，两处说法就打架了。
pub async fn report(settings: &crate::config::SyncConfig) -> Report {
    let distro = Distro::detect();

    // rclone 算不算必需项,取决于用户选的引擎:引擎是 kopia 时那条路根本不会走到
    // rclone,把它报成"缺了就不行"会让报告一直是红的(而 Windows 包里内置的正是 kopia)。
    let rclone_level = match settings.engine {
        crate::config::SyncEngine::Rclone => Level::Required,
        crate::config::SyncEngine::Kopia => Level::Optional,
    };

    // gamescope / wine / 窗口尺寸控制属于 Linux 那条「kotori 自己把游戏拉起来、用
    // gamescope 缩放」的路。Windows 版不走那条路(它借 Magpie,而且只能观察),在这儿报
    // 「缺 wine」「只在 KDE Plasma 上实现」只会让用户以为自己少装了什么 —— 所以是
    // **整个不报**,而不是把结论改成「缺失」。三个平台各有各的清单,不做交集。
    #[cfg(unix)]
    let checks = vec![
        probes::gamescope(&distro).await,
        probes::wine(&distro).await,
        probes::rclone(&distro, &settings.rclone_binary, rclone_level).await,
        probes::kopia(&distro, &settings.kopia_binary).await,
        probes::file_dialog(&distro).await,
        probes::window_control(),
        probes::resolution(),
        probes::keyring(&distro),
    ];

    #[cfg(windows)]
    let checks = vec![
        probes::rclone(&distro, &settings.rclone_binary, rclone_level).await,
        probes::kopia(&distro, &settings.kopia_binary).await,
        probes::file_dialog(&distro).await,
        probes::resolution(),
        probes::keyring(&distro),
    ];

    Report::from_checks(checks)
}

#[cfg(test)]
mod tests {
    use super::distro::{Distro, Family, Package};
    use super::probes::first_line;
    #[cfg(unix)]
    use super::probes::gamescope_version;
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
        // 没有系统文件对话框、平铺桌面上不能改窗口尺寸、没装 kopia —— 这些都是能用的机器。
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
            debian.install(Package::per_distro(
                "libsecret",
                "libsecret-tools",
                "libsecret"
            )),
            "sudo apt install libsecret-tools"
        );

        let fedora = Distro::parse("ID=fedora\nPRETTY_NAME=\"Fedora 41\"\n");
        assert_eq!(
            fedora.install(Package::same("rclone")),
            "sudo dnf install rclone"
        );

        // kopia 在 Arch 官方仓库里没有,命令必须指向 archlinuxcn。
        assert_eq!(
            arch.install(Package::per_distro("archlinuxcn/kopia", "kopia", "kopia")),
            "sudo pacman -S archlinuxcn/kopia"
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
        // kopia 的 `--version` 也是一行,原样可用。
        assert_eq!(
            first_line("0.22.3 build: xyz\n", "已安装"),
            "0.22.3 build: xyz"
        );
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[test]
    fn kopia_is_an_alternative_not_a_requirement() {
        // 缺 kopia 不让整份报告失败:rclone 那条路照常,用户只是少一个选择。
        // 反过来也一样 —— 两条路各是一个"方式",不是彼此的依赖。
        let report = Report::from_checks(vec![
            check(Level::Required, State::Ready),
            check(Level::Optional, State::Missing),
        ]);
        assert!(report.ok, "{report:?}");
        assert_eq!(
            Check::missing_optional("kopia", "kopia", "x", String::new()).level,
            Level::Optional
        );
        assert_eq!(
            Check::ready_optional("kopia", "kopia", "x".to_string(), "y").state,
            State::Ready
        );
    }
}

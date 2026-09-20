//! 本机环境:Wine prefix 的情况,以及设置页「环境检查」的一份结果。
//!
//! 只被 `env.report` / `daemon.status` 的解析和设置页使用,不含任何游戏或云同步
//! 的状态 —— 所以它自己一个文件,谁问「这台机器行不行」都从这里取。

/// Wine prefix situation on this machine (settings page).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WineStatus {
    pub configured: Option<String>,
    pub default_prefix: String,
    pub environment: Option<String>,
    pub detected: Vec<String>,
}

/// 配置文件落在哪儿、能不能换(设置页「配置文件」那一组)。
///
/// 只有两个地点(用户 2026-09-19):**二进制同目录**(便携)与**平台默认目录**;
/// 启动时便携优先。谁在生效由 daemon 报 —— 界面自己那份 `config::config_path()`
/// 算出来的可能与 daemon 记的不一样(它启动时就钉住了那一个),以 daemon 为准。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSource {
    /// 现在生效的那一份。
    pub path: String,
    /// 便携地点(二进制同目录的 `config.toml`);取不到二进制目录时是 `None`。
    pub portable_path: Option<String>,
    /// 路径被 `KOTORI_CONFIG` 钉死了 —— 切换没有意义。
    pub pinned: bool,
}

impl ConfigSource {
    /// 现在这一份是不是便携那份。
    pub fn is_portable(&self) -> bool {
        self.portable_path.as_deref() == Some(self.path.as_str())
    }

    /// 一句话说清它现在在哪。
    pub fn label(&self) -> &'static str {
        if self.pinned {
            "由环境变量 KOTORI_CONFIG 指定"
        } else if self.is_portable() {
            "便携（kotori 同目录）"
        } else {
            "平台默认目录"
        }
    }

    /// 能不能切:路径没被钉死、也知道便携地点在哪、而且 daemon 报过话。
    pub fn can_switch(&self) -> bool {
        !self.pinned && self.portable_path.is_some() && !self.path.is_empty()
    }
}

/// 设置页「环境检查」的一行,来自 `env.report`(探测本身在 `crate::platform`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvCheck {
    pub title: String,
    /// `0` 可用 / `1` 有条件 / `2` 缺少 —— 页面的颜色由它决定(与 `state_label` 同源)。
    pub state: i32,
    pub state_label: String,
    /// 现在是什么情况(版本 / 路径 / 为什么退化)。
    pub detail: String,
    /// 少了它就没有什么,或者退化成什么。
    pub impact: String,
    /// 装它的命令;空 = 没什么可装的。
    pub install: String,
    /// 没有它就算机器"不能用"(决定顶上那句话是绿的还是红的)。
    pub required: bool,
}

/// 一整份环境检查。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub distro: String,
    pub ok: bool,
    pub checks: Vec<EnvCheck>,
}

impl Environment {
    /// 顶上那一行话。措辞在这里定,页面只显示(与 `service_state` 同一个做法)。
    pub fn summary(&self) -> String {
        if self.checks.is_empty() {
            return "还没有检查结果".to_string();
        }
        let missing: Vec<&str> = self
            .checks
            .iter()
            .filter(|check| check.required && check.state == 2)
            .map(|check| check.title.as_str())
            .collect();
        if !missing.is_empty() {
            return format!(
                "缺少 {} 项必需的依赖:{} —— 装好它才能正常用",
                missing.len(),
                missing.join("、")
            );
        }
        let degraded = self.checks.iter().filter(|check| check.state != 0).count();
        if degraded == 0 {
            "必需项与可选功能都就绪".to_string()
        } else {
            format!("必需项都在;{degraded} 项可选功能退化了(下面写了退化成什么)")
        }
    }

    /// 「发行版:Arch Linux」这一行;认不出来就是空。
    pub fn distro_line(&self) -> String {
        if self.distro.is_empty() {
            String::new()
        } else {
            format!("发行版:{}", self.distro)
        }
    }
}

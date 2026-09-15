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

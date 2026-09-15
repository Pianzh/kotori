//! `env.report`:设置页那一组「环境检查」。
//!
//! 与邻居分开的理由:这里的输入是 `env.report` 的回包(发行版 + 每条检查的
//! state/detail/impact/install);Wine 前缀来自 `daemon.status`,云同步来自
//! `sync.status` —— 各是一条请求,各解析各的。

use super::*;

/// `env.report`:设置页那一组「环境检查」。
///
/// ⚠ 缺字段一律按**最坏**算(`state` 认不出来就是"缺少"),别让一个残缺的回包
/// 显示成"一切正常" —— 这一页存在的理由就是"界面不许撒谎"。
pub(in crate::ui) fn parse_environment(value: &Value) -> Environment {
    let checks = value
        .get("checks")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(parse_check).collect())
        .unwrap_or_default();
    Environment {
        distro: str_field(value, "distro"),
        ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
        checks,
    }
}

fn parse_check(row: &Value) -> EnvCheck {
    let state = match row.get("state").and_then(Value::as_str) {
        Some("ready") => 0,
        Some("degraded") => 1,
        _ => 2,
    };
    EnvCheck {
        title: str_field(row, "title"),
        state,
        state_label: match state {
            0 => "可用",
            1 => "有条件",
            _ => "缺少",
        }
        .to_string(),
        detail: str_field(row, "detail"),
        impact: str_field(row, "impact"),
        install: str_field(row, "install"),
        required: row.get("level").and_then(Value::as_str) == Some("required"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_environment_report_keeps_its_wording_in_rust() {
        let environment = parse_environment(&json!({
            "platform": "linux",
            "distro": "Arch Linux",
            "ok": true,
            "checks": [
                {"id":"gamescope","title":"gamescope","level":"required","state":"ready",
                 "detail":"gamescope version 3.16.28","impact":"启动游戏与缩放增强","install":""},
                {"id":"window-control","title":"窗口尺寸控制","level":"optional","state":"degraded",
                 "detail":"只在 KDE Plasma 上实现","impact":"改窗口尺寸会回「做不到」","install":""},
            ],
        }));
        assert_eq!(environment.distro_line(), "发行版:Arch Linux");
        assert_eq!(environment.checks[0].state_label, "可用");
        assert_eq!(environment.checks[0].state, 0);
        assert_eq!(environment.checks[1].state_label, "有条件");
        assert!(environment.checks[0].required);
        assert!(!environment.checks[1].required);
        // 必需项都在、只有可选退化 ⇒ 绿字,且说实话。
        assert_eq!(
            environment.summary(),
            "必需项都在;1 项可选功能退化了(下面写了退化成什么)"
        );
    }

    #[test]
    fn a_missing_required_dependency_is_named_in_the_summary() {
        let environment = parse_environment(&json!({
            "distro": "Arch Linux",
            "ok": false,
            "checks": [
                {"id":"rclone","title":"rclone","level":"required","state":"missing",
                 "detail":"没找到","impact":"没有它就没有云存档同步","install":"sudo pacman -S rclone"},
            ],
        }));
        assert_eq!(
            environment.summary(),
            "缺少 1 项必需的依赖:rclone —— 装好它才能正常用"
        );
    }

    #[test]
    fn an_unknown_state_is_read_as_missing_not_as_fine() {
        // 回包残缺时宁可说"缺少":这一页存在的理由就是界面不许撒谎。
        let environment = parse_environment(&json!({"checks": [{"title": "x"}]}));
        assert_eq!(environment.checks[0].state, 2);
        assert_eq!(environment.checks[0].state_label, "缺少");
        assert!(!environment.ok);
        // 一个字段都没有时也不能说"一切就绪"。
        let empty = parse_environment(&json!({}));
        assert!(empty.checks.is_empty());
        assert_eq!(empty.summary(), "还没有检查结果");
        assert_eq!(empty.distro_line(), "");
    }
}

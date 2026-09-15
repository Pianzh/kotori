//! `daemon.status` 里的 Wine 情况:配置里指定的、默认的、以及这台机器上探测到的。
//!
//! 单独一条线:它只服务设置页的 Wine 那一块,和 `sync.status` 的回包没有一个
//! 共同字段,拼在一起只会让两边都难改。

use super::*;

/// `daemon.status` 里的 wine 情况。
pub(in crate::ui) fn parse_wine_status(value: &Value) -> WineStatus {
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    WineStatus {
        configured: text("configured"),
        default_prefix: text("default").unwrap_or_default(),
        environment: text("environment"),
        detected: value
            .get("detected")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_wine_status() {
        let value = json!({
            "configured": "/prefixes/games",
            "default": "/home/user/.wine",
            "environment": null,
            "detected": ["/home/user/.local/share/wineprefixes/a", "/home/user/.wine"]
        });
        let status = parse_wine_status(&value);
        assert_eq!(status.configured.as_deref(), Some("/prefixes/games"));
        assert_eq!(status.default_prefix, "/home/user/.wine");
        assert_eq!(status.environment, None);
        assert_eq!(status.detected.len(), 2);

        // A daemon that reports nothing usable still yields a sane value.
        let empty = parse_wine_status(&json!({}));
        assert_eq!(empty.configured, None);
        assert!(empty.detected.is_empty());
    }
}

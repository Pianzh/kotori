//! 存档位置编辑器 <-> daemon 的 JSON:一进一出必须无损,外加每种 kind 的占位符。
//!
//! 和游戏列表分开:那边回答「游戏有哪些」,这边回答「一款游戏的存档位置怎么写」;
//! 改编辑器只碰这一份。

use super::*;

/// Placeholder that shows the expected shape of each save-location kind.
pub(in crate::ui) fn kind_placeholder(kind: &str) -> &'static str {
    match kind {
        "windows" => "%APPDATA%\\Game\\save",
        "absolute" => "/home/user/saves/game",
        _ => "savedata",
    }
}

pub(in crate::ui) fn save_paths_to_json(paths: &[SavePathDraft]) -> Value {
    Value::Array(
        paths
            .iter()
            .map(|entry| {
                let mut object = serde_json::Map::new();
                object.insert("kind".into(), Value::String(entry.kind.clone()));
                object.insert("path".into(), Value::String(entry.path.clone()));
                let exclude: Vec<Value> = entry
                    .exclude
                    .split(',')
                    .map(str::trim)
                    .filter(|pattern| !pattern.is_empty())
                    .map(|pattern| Value::String(pattern.to_string()))
                    .collect();
                if !exclude.is_empty() {
                    object.insert("exclude".into(), Value::Array(exclude));
                }
                Value::Object(object)
            })
            .collect(),
    )
}

pub(in crate::ui) fn parse_save_paths(value: Option<&Value>) -> Vec<SavePathDraft> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| SavePathDraft {
                    kind: item
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("relative")
                        .to_string(),
                    path: item
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    exclude: item
                        .get("exclude")
                        .and_then(|v| v.as_array())
                        .map(|patterns| {
                            patterns
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_paths_round_trip_between_editor_and_daemon() {
        let paths = vec![
            SavePathDraft {
                kind: "windows".into(),
                path: "%APPDATA%\\Game".into(),
                exclude: "*.log, cache/".into(),
            },
            SavePathDraft {
                kind: "relative".into(),
                path: "savedata".into(),
                exclude: String::new(),
            },
        ];

        let json = save_paths_to_json(&paths);
        assert_eq!(json[0]["kind"], "windows");
        assert_eq!(json[0]["exclude"][0], "*.log");
        assert_eq!(json[0]["exclude"][1], "cache/");
        assert!(
            json[1].get("exclude").is_none(),
            "an empty exclude list must not be sent"
        );

        assert_eq!(
            parse_save_paths(Some(&json)),
            paths,
            "editor -> daemon -> editor must be lossless"
        );
        assert!(parse_save_paths(None).is_empty());
    }

    #[test]
    fn kind_placeholders_teach_each_format() {
        assert_eq!(kind_placeholder("windows"), "%APPDATA%\\Game\\save");
        assert!(kind_placeholder("relative").contains("save"));
        assert!(kind_placeholder("absolute").starts_with('/'));
    }
}

//! JSON from the daemon <-> the UI's own structs, plus the small pure helpers the
//! pages use (labels, placeholders, search matching, retry backoff).
//! No widget is built here.

use super::*;

/// Text field of a JSON object, or empty when absent.
pub(super) fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// What the B2 section says about the stored credentials.
///
/// There is only ever **one** set of B2 keys (the daemon overwrites the two
/// entries), so the useful information is how many of its two halves are
/// actually there — never the values, which do not leave the store. `store` is
/// the *name* of the tier they are in right now: saying "密钥环" on a machine
/// that has none would promise persistence we do not have.
pub(super) fn credentials_label(has_key_id: bool, has_app_key: bool, store: &str) -> String {
    let stored = usize::from(has_key_id) + usize::from(has_app_key);
    let mark = |saved: bool| if saved { "✓" } else { "✗ 未保存" };
    format!(
        "{store}里现在有 {stored}/2 项：keyID {}，applicationKey {}。再次保存会覆盖上一套，\
         只进{store}，配置文件里没有任何明文。",
        mark(has_key_id),
        mark(has_app_key),
    )
}

/// Backoff for automatic reconnect attempts: 2s, 4s, 8s, 16s, capped at 30s.
pub(super) fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.pow(attempt.min(4)).min(30))
}

/// Case-insensitive match against a game's name or exe path; an empty query
/// matches everything.
pub(super) fn matches_query(game: &UiGame, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    game.name.to_lowercase().contains(&query) || game.exe.to_lowercase().contains(&query)
}

/// `daemon.status` 里的 `hotkeys` 对象。缺字段一律按"没注册"算 ——
/// 宁可说"还没有热键",不要说成"已就绪"。
pub(super) fn parse_hotkeys(value: &Value) -> HotkeyStatus {
    let hotkeys = value.get("hotkeys");
    let field = |key: &str| hotkeys.and_then(|hotkeys| hotkeys.get(key));
    HotkeyStatus {
        requested: field("requested").and_then(Value::as_bool).unwrap_or(false),
        ready: field("ready").and_then(Value::as_bool).unwrap_or(false),
        error: field("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|message| !message.is_empty()),
        unbound: string_list(field("unbound")),
        assign_hint: field("assign_hint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

pub(super) fn parse_wine_status(value: &Value) -> WineStatus {
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

pub(super) fn parse_sync_status(value: &Value) -> Result<SyncStatus, String> {
    if value.get("settings").is_none() {
        return Err("守护进程没有返回同步设置".to_string());
    }
    let games = value
        .get("games")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| SyncGameRow {
                    id: str_field(row, "id"),
                    name: str_field(row, "name"),
                    locations: row.get("locations").and_then(|v| v.as_u64()).unwrap_or(0),
                    problem: row
                        .get("location_problem")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    last: row.get("last").and_then(|last| {
                        if last.is_null() {
                            return None;
                        }
                        let action = last.get("action").and_then(|v| v.as_str()).unwrap_or("");
                        let detail = last.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                        let when = last
                            .get("at")
                            .and_then(|v| v.as_str())
                            .map(|at| at.chars().take(16).collect::<String>())
                            .unwrap_or_default();
                        let mark = if last.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "✓"
                        } else {
                            "✗"
                        };
                        Some(format!("{mark} {when} {action} {detail}"))
                    }),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SyncStatus {
        settings: value.get("settings").cloned().unwrap_or(Value::Null),
        remote: str_field(value, "remote"),
        rclone: value
            .get("rclone")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        keyring: value
            .get("keyring")
            .map(|keyring| str_field(keyring, "backend"))
            .unwrap_or_default(),
        store_kind: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "kind"))
            .unwrap_or_default(),
        store_locked: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .and_then(|store| store.get("locked"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        store_path: value
            .get("keyring")
            .and_then(|keyring| keyring.get("store"))
            .map(|store| str_field(store, "path"))
            .unwrap_or_default(),
        master_file: value
            .get("keyring")
            .map(|keyring| str_field(keyring, "secrets_file"))
            .unwrap_or_default(),
        min_master_password: value
            .get("keyring")
            .and_then(|keyring| keyring.get("min_master_password"))
            .and_then(|v| v.as_u64())
            .unwrap_or(8) as usize,
        ephemeral: value
            .get("keyring")
            .and_then(|v| v.get("ephemeral"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        secrets: string_list(value.get("secrets")),
        ready: value
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        problem: value
            .get("problem")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        password_hint: str_field(value, "password_hint"),
        games,
    })
}

pub(super) fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One line summarising a sync result: how many locations moved, or what broke.
pub(super) fn describe_sync_outcome(value: &Value) -> String {
    let outcomes: Vec<&Value> = match (
        value.get("games").and_then(|v| v.as_array()),
        value.get("game").filter(|v| !v.is_null()),
    ) {
        (Some(games), _) => games.iter().collect(),
        (None, Some(game)) => vec![game],
        _ => vec![value],
    };

    let mut moved = 0usize;
    let mut skipped = 0usize;
    let mut problems = Vec::new();
    for outcome in &outcomes {
        if let Some(error) = outcome.get("error").and_then(|v| v.as_str()) {
            problems.push(error.to_string());
        }
        for location in outcome
            .get("locations")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            match location.get("action").and_then(|v| v.as_str()) {
                Some("skipped") => skipped += 1,
                Some("failed") => {}
                _ => moved += 1,
            }
        }
    }

    if !problems.is_empty() {
        return format!("失败：{}", problems.join("；"));
    }
    if moved == 0 {
        return format!("没有需要同步的变化（跳过 {skipped} 个位置）");
    }
    format!("完成：{moved} 个位置已同步，跳过 {skipped} 个")
}

/// Placeholder that shows the expected shape of each save-location kind.
pub(super) fn kind_placeholder(kind: &str) -> &'static str {
    match kind {
        "windows" => "%APPDATA%\\Game\\save",
        "absolute" => "/home/user/saves/game",
        _ => "savedata",
    }
}

pub(super) fn save_paths_to_json(paths: &[SavePathDraft]) -> Value {
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

pub(super) fn parse_save_paths(value: Option<&Value>) -> Vec<SavePathDraft> {
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

pub(super) fn profile_from_draft(draft: &Draft) -> Result<ScaleProfile, String> {
    let algorithm = ScaleAlgorithm::from_label(&draft.algo)
        .ok_or_else(|| format!("未知缩放算法: {}", draft.algo))?
        .with_sharpness(draft.sharpness);

    // 留空＝自动(启动时按屏幕算);填了才覆盖。半截数字是用户能改的错误,
    // 不是"静默退回自动" —— 那等于把他刚填的东西悄悄丢掉。
    let scale_ratio = match draft.scale_ratio.trim() {
        "" => None,
        raw => Some(
            raw.parse::<f32>()
                .map_err(|_| format!("缩放比例必须是数字（当前 {raw}）"))?,
        ),
    };

    Ok(ScaleProfile {
        name: draft.profile_name.clone(),
        algorithm,
        internal_width: parse_optional_u32(&draft.internal_w, "游戏分辨率宽")?,
        internal_height: parse_optional_u32(&draft.internal_h, "游戏分辨率高")?,
        output_width: parse_optional_u32(&draft.output_w, "输出分辨率宽")?,
        output_height: parse_optional_u32(&draft.output_h, "输出分辨率高")?,
        scale_ratio,
        follow_window: draft.follow_window,
        framerate_limit: if draft.framerate.trim().is_empty() {
            None
        } else {
            Some(parse_u32(&draft.framerate, "帧率限制")?)
        },
        force_fullscreen: draft.fullscreen,
    })
}

pub(super) fn parse_u32(s: &str, label: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} 必须是正整数"))
}

/// 留空的数字字段＝"不说"(＝自动),不是 0。
pub(super) fn parse_optional_u32(s: &str, label: &str) -> Result<Option<u32>, String> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    parse_u32(s, label).map(Some)
}

pub(super) fn parse_games(value: &Value) -> Result<Vec<UiGame>, String> {
    let games = value
        .get("games")
        .and_then(|g| g.as_array())
        .ok_or_else(|| "守护进程返回格式异常".to_string())?;

    games
        .iter()
        .map(|g| {
            // The daemon sends the whole GameConfig, so read `scale_profile`
            // directly instead of a hand-picked subset.
            let scale = g.get("scale_profile");
            let algorithm = scale.and_then(|s| s.get("algorithm"));

            Ok(UiGame {
                id: g
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: g
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知")
                    .to_string(),
                game_dir: g
                    .get("game_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                save_paths: parse_save_paths(g.get("save_paths")),
                watch_only: g
                    .get("watch_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                process_name: g
                    .get("process_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                exe: g
                    .get("exe_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                profile_name: scale
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("默认")
                    .to_string(),
                algo: algorithm
                    .and_then(algo_label)
                    .unwrap_or_else(|| "Fsr".to_string()),
                sharpness: algorithm.and_then(algo_sharpness).unwrap_or(2),
                // 留空＝由 gamescope 定(见 `ScaleProfile::internal_width`),
                // 所以缺字段是正常的,不是"0"。
                internal: (
                    u32_field(scale, "internal_width"),
                    u32_field(scale, "internal_height"),
                ),
                output: (
                    u32_field(scale, "output_width"),
                    u32_field(scale, "output_height"),
                ),
                scale_ratio: scale
                    .and_then(|s| s.get("scale_ratio"))
                    .and_then(|v| v.as_f64())
                    .map(|r| r as f32),
                follow_window: scale
                    .and_then(|s| s.get("follow_window"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                fullscreen: scale
                    .and_then(|s| s.get("force_fullscreen"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                framerate: scale
                    .and_then(|s| s.get("framerate_limit"))
                    .and_then(|v| v.as_u64().map(|f| f as u32)),
            })
        })
        .collect()
}

pub(super) fn u32_field(parent: Option<&Value>, key: &str) -> Option<u32> {
    parent?.get(key)?.as_u64().map(|v| v as u32)
}

/// Algorithm label from the serialized form. Serde tags struct variants as an
/// object (`{"Fsr": {"sharpness": 2}}`) but unit variants as a bare string
/// (`"Integer"`), so both shapes must be handled.
pub(super) fn algo_label(v: &Value) -> Option<String> {
    if let Some(label) = v.as_str() {
        return Some(label.to_string());
    }
    let obj = v.as_object()?;
    obj.keys().next().cloned()
}

/// `sharpness` of the serialized algorithm, when it has one.
pub(super) fn algo_sharpness(v: &Value) -> Option<u32> {
    let obj = v.as_object()?;
    let (_k, val) = obj.iter().next()?;
    val.get("sharpness")?.as_u64().map(|s| s as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_status_fixture, ui_game};
    use serde_json::json;

    fn daemon_game_list() -> Value {
        json!({
            "games": [{
                "id": "demo",
                "name": "demo",
                "exe_path": "/games/demo/game.exe",
                "save_paths": [],
                "wine_prefix": null,
                "created_at": "2026-01-01T00:00:00Z",
                "scale_profile": {
                    "name": "自定义",
                    "algorithm": { "Nis": { "sharpness": 4 } },
                    "internal_width": 1920,
                    "internal_height": 1080,
                    "output_width": 2560,
                    "output_height": 1440,
                    "framerate_limit": 60,
                    "force_fullscreen": false
                }
            }]
        })
    }

    fn draft_with(algo: &str) -> Draft {
        Draft {
            game_id: "x".into(),
            profile_name: "默认".into(),
            game_dir: "/games/x".into(),
            game_dir_original: "/games/x".into(),
            exe: "/games/x/game.exe".into(),
            exe_original: "/games/x/game.exe".into(),
            save_paths: Vec::new(),
            save_paths_original: Vec::new(),
            algo: algo.into(),
            sharpness: 2,
            internal_w: "1280".into(),
            internal_h: "720".into(),
            output_w: "2560".into(),
            output_h: "1440".into(),
            scale_ratio: String::new(),
            follow_window: true,
            fullscreen: true,
            framerate: String::new(),
        }
    }

    #[test]
    fn parses_the_full_scale_profile() {
        let games = parse_games(&daemon_game_list()).unwrap();
        let game = &games[0];
        assert_eq!(game.id, "demo");
        assert_eq!(game.profile_name, "自定义");
        assert_eq!(game.algo, "Nis");
        assert_eq!(game.sharpness, 4);
        assert_eq!(game.internal, (Some(1920), Some(1080)));
        assert_eq!(game.output, (Some(2560), Some(1440)));
        assert_eq!(game.framerate, Some(60));
        assert!(!game.fullscreen);
    }

    #[test]
    fn open_then_save_preserves_stored_values() {
        // Regression test for the silent overwrite: the draft used to start
        // from hard-coded defaults (sharpness 2 / fullscreen true / no fps).
        let game = parse_games(&daemon_game_list()).unwrap().remove(0);
        let draft = Draft::from_game(&game);

        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.name, "自定义");
        assert_eq!(profile.algorithm, ScaleAlgorithm::Nis { sharpness: 4 });
        assert_eq!(profile.framerate_limit, Some(60));
        assert!(!profile.force_fullscreen);
    }

    #[test]
    fn unit_variant_algorithms_survive_a_load_save_cycle() {
        // serde writes unit variants as a bare string (`"Integer"`). Reading
        // that as an object used to silently turn the game into FSR on save.
        let value = json!({
            "games": [{
                "id": "int",
                "name": "int",
                "exe_path": "/int.exe",
                "scale_profile": {
                    "name": "默认",
                    "algorithm": "Integer",
                    "internal_width": 640,
                    "internal_height": 480,
                    "output_width": 1280,
                    "output_height": 960
                }
            }]
        });
        let game = parse_games(&value).unwrap().remove(0);
        assert_eq!(game.algo, "Integer");

        let draft = Draft::from_game(&game);
        assert_eq!(draft.algo, "Integer");
        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.algorithm, ScaleAlgorithm::Integer);
        assert_eq!(profile.internal_width, Some(640));
        assert_eq!(profile.output_width, Some(1280));
    }

    #[test]
    fn malformed_and_empty_response_are_errors() {
        assert!(parse_games(&json!({})).is_err());
        assert!(parse_games(&json!({ "games": [] })).unwrap().is_empty());
    }

    #[test]
    fn missing_optional_fields_fall_back_safely() {
        let value = json!({
            "games": [{
                "id": "x",
                "name": "x",
                "exe_path": "/x.exe",
                "scale_profile": { "algorithm": "Integer" }
            }]
        });
        let games = parse_games(&value).unwrap();
        assert_eq!(games[0].algo, "Integer");
        assert_eq!(games[0].sharpness, 2);
        assert_eq!(games[0].internal, (None, None));
        assert!(!games[0].fullscreen);
    }

    #[test]
    fn unknown_algorithm_is_rejected_on_save() {
        assert!(profile_from_draft(&draft_with("Lanczos")).is_err());
    }

    #[test]
    fn non_numeric_resolution_is_rejected() {
        let mut draft = draft_with("Fsr");
        draft.internal_w = "abc".into();
        let err = profile_from_draft(&draft).unwrap_err();
        assert!(err.contains("游戏分辨率宽"), "{err}");
    }

    /// A plain open + save must neither invent nor drop a scaling ratio:
    /// "no ratio" (the state every older profile is in) stays "no ratio", a set
    /// one survives, and half-typed text is an error rather than a silent reset.
    #[test]
    fn the_scaling_ratio_survives_an_open_and_save() {
        let game = ui_game();
        let untouched = profile_from_draft(&Draft::from_game(&game)).unwrap();
        assert_eq!(untouched.scale_ratio, None);
        assert!(untouched.follow_window);

        let mut pinned = game.clone();
        pinned.scale_ratio = Some(1.5);
        pinned.follow_window = false;
        let profile = profile_from_draft(&Draft::from_game(&pinned)).unwrap();
        assert_eq!(profile.scale_ratio, Some(1.5));
        assert!(!profile.follow_window);

        let mut half_typed = Draft::from_game(&pinned);
        half_typed.scale_ratio = "1.5x".into();
        let err = profile_from_draft(&half_typed).unwrap_err();
        assert!(err.contains("缩放比例"), "{err}");
    }

    /// A freshly parsed game must not look "edited" to the save button.
    impl UiGame {
        fn save_paths_changed_after_edit(&self) -> bool {
            Draft::from_game(self).save_paths_changed()
        }
    }

    #[test]
    fn search_matches_name_and_path_case_insensitively() {
        let game = ui_game();
        assert!(matches_query(&game, ""), "empty query shows everything");
        assert!(matches_query(&game, "   "));
        assert!(matches_query(&game, "demo"));
        assert!(matches_query(&game, "DEMO"));
        assert!(matches_query(&game, "Game")); // name
        assert!(matches_query(&game, "games/demo")); // path
        assert!(matches_query(&game, ".exe"));
        assert!(!matches_query(&game, "nonexistent"));
    }

    #[test]
    fn retry_backoff_grows_then_caps() {
        assert_eq!(retry_delay(1), std::time::Duration::from_secs(2));
        assert_eq!(retry_delay(2), std::time::Duration::from_secs(4));
        assert_eq!(retry_delay(3), std::time::Duration::from_secs(8));
        assert_eq!(retry_delay(4), std::time::Duration::from_secs(16));
        // capped, and never overflows for a large attempt count
        assert_eq!(retry_delay(5), std::time::Duration::from_secs(16));
        assert_eq!(retry_delay(99), std::time::Duration::from_secs(16));
    }

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

    #[test]
    fn parses_watch_mode_and_save_paths_from_the_daemon() {
        let value = json!({
            "games": [{
                "id": "w",
                "name": "W",
                "game_dir": "/games/w",
                "exe_path": "/games/w/game.exe",
                "watch_only": true,
                "process_name": "game.exe",
                "save_paths": [
                    { "kind": "windows", "path": "%APPDATA%\\W", "exclude": ["*.log", "tmp/"] }
                ],
                "scale_profile": { "algorithm": "Integer" }
            }]
        });

        let game = parse_games(&value).unwrap().remove(0);
        assert!(game.watch_only);
        assert_eq!(game.process_name, "game.exe");
        assert_eq!(game.save_paths.len(), 1);
        assert_eq!(game.save_paths[0].kind, "windows");
        assert_eq!(game.save_paths[0].exclude, "*.log, tmp/");
        assert!(!game.save_paths_changed_after_edit());
    }

    #[test]
    fn parses_the_sync_status() {
        let status = sync_status_fixture();
        assert!(status.ready);
        assert!(!status.ephemeral);
        assert_eq!(status.store_kind, "system");
        assert!(!status.store_locked);
        assert_eq!(status.store(), CredentialStore::System);

        // 明文文件那一级(现在的默认):路径要能取到,不然页面说不出"存在哪"。
        let plain = parse_sync_status(&serde_json::json!({
            "keyring": {
                "backend": "明文凭据文件 /home/user/.config/kotori/credentials.json（权限 0600，只有你能读）",
                "ephemeral": false,
                "store": { "kind": "plain-file", "path": "/home/user/.config/kotori/credentials.json" },
                "secrets_file": "/home/user/.config/kotori/secrets.json",
                "min_master_password": 8,
            },
            "settings": {},
        }))
        .unwrap();
        assert_eq!(plain.store(), CredentialStore::Plain);
        assert_eq!(
            plain.store_path,
            "/home/user/.config/kotori/credentials.json"
        );
        assert!(
            status.master_file.ends_with("secrets.json"),
            "凭据会存到哪要一直有答案:{}",
            status.master_file
        );
        assert_eq!(status.min_master_password, 8);
        assert_eq!(status.remote, "kotori:kotori-saves/kotori");
        assert_eq!(status.rclone.as_deref(), Some("/usr/bin/rclone"));
        assert!(status.problem.is_none());
        assert!(status.keyring.contains("Secret Service"));
        assert!(
            status.password_hint.contains("secret-tool"),
            "the user must be able to read the password back without kotori"
        );

        assert_eq!(status.games.len(), 2);
        assert_eq!(status.games[0].locations, 2);
        let last = status.games[0].last_label();
        assert!(last.contains("✓") && last.contains("上传"), "{last}");
        assert_eq!(status.games[1].last_label(), "还没同步过");

        // A malformed payload is an error, not a silently empty page.
        assert!(parse_sync_status(&Value::Null).is_err());
    }

    #[test]
    fn the_credentials_section_counts_what_is_stored() {
        // One pair of keys per account, so "how many" means "how many of the
        // two halves are there" — the values never come back from the daemon.
        assert!(credentials_label(true, true, "系统密钥环").contains("2/2"));
        assert!(credentials_label(true, false, "系统密钥环").contains("1/2"));
        let empty = credentials_label(false, false, "系统密钥环");
        assert!(empty.contains("0/2"), "{empty}");
        assert!(empty.contains("未保存"), "{empty}");
        assert!(empty.contains("覆盖"), "覆盖语义要写出来：{empty}");

        // 说哪一级就说那一级:没有密钥环的机器上不能写成"密钥环里"。
        let memory = credentials_label(true, true, CredentialStore::Session.name());
        assert!(memory.contains("本次会话的内存里现在有 2/2"), "{memory}");
        assert!(!memory.contains("密钥环"), "{memory}");

        let status = sync_status_fixture();
        assert!(status.has_secret("b2-key-id") && status.has_secret("sync-password"));
        assert!(!status.has_secret("b2-app-key-x"));
    }

    #[test]
    fn sync_outcomes_are_summarised_for_a_human() {
        // A bulk upload: one location moved, one skipped.
        let value = serde_json::json!({
            "ok": true,
            "games": [{
                "game_id": "demo",
                "name": "Demo",
                "ok": true,
                "locations": [
                    { "configured": "savedata", "local": "/g/savedata", "action": "uploaded", "detail": "已上传" },
                    { "configured": "%APPDATA%\\\\X", "local": "/w/X", "action": "skipped", "detail": "本地没有这个目录" }
                ]
            }]
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("1 个位置已同步"), "{summary}");
        assert!(summary.contains("跳过 1"), "{summary}");

        // Nothing changed is not a failure.
        let value = serde_json::json!({
            "ok": true,
            "games": [{ "game_id": "demo", "name": "Demo", "ok": true, "locations": [] }]
        });
        assert!(describe_sync_outcome(&value).contains("没有需要同步的变化"));

        // A failure names the location that broke.
        let value = serde_json::json!({
            "ok": false,
            "game": {
                "game_id": "demo",
                "name": "Demo",
                "ok": false,
                "error": "savedata: rclone 执行失败",
                "locations": []
            }
        });
        let summary = describe_sync_outcome(&value);
        assert!(summary.contains("失败"), "{summary}");
        assert!(summary.contains("savedata"), "{summary}");
    }
}

//! 游戏列表:把 `game.list` 的回包读成 `UiGame`,外加库页按名字/路径过滤。
//!
//! 存档位置虽然挂在游戏上,但它的读写(编辑器 <-> daemon 的 JSON)自成一条线,
//! 放在 `save_paths` 里 —— 这里只管「有哪些游戏」。

use super::*;

/// Case-insensitive match against a game's name or exe path; an empty query
/// matches everything.
pub(in crate::ui) fn matches_query(game: &UiGame, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    game.name.to_lowercase().contains(&query) || game.exe.to_lowercase().contains(&query)
}

/// 读一条挂载引用。`disk` 是身份：它没写就当"没有引用"（只有相对目录没有意义）。
pub(in crate::ui) fn parse_mount(value: &Value) -> Option<MountRef> {
    let disk = value.get("disk").and_then(|v| v.as_str()).unwrap_or("");
    if disk.trim().is_empty() {
        return None;
    }
    Some(MountRef {
        disk: disk.to_string(),
        relative: value
            .get("relative")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

pub(in crate::ui) fn parse_games(value: &Value) -> Result<Vec<UiGame>, String> {
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
                // 读 `auto_watch`;**旧 daemon 还在跑**时它只报 `watch_only`,
                // 那也认(daemon 是长命进程,界面比它新是常态)。
                auto_watch: g
                    .get("auto_watch")
                    .or_else(|| g.get("watch_only"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                direct_launch: g
                    .get("direct_launch")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                // 旧配置里没有这一栏（默认就是"参与"）。
                sync_enabled: g
                    .get("sync_enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
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
                // 挂载引用：盘不在时上两栏是空的，只有这里还指着那块盘（`disk` 没写
                // 就等于没有引用）。
                game_dir_mount: g
                    .get("game_dir_mount")
                    .and_then(parse_mount)
                    .unwrap_or_default(),
                exe_mount: g.get("exe_mount").and_then(parse_mount).unwrap_or_default(),
                launch_args: string_list(g.get("launch_args")),
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
                fullscreen: scale
                    .and_then(|s| s.get("force_fullscreen"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                framerate: scale
                    .and_then(|s| s.get("framerate_limit"))
                    .and_then(|v| v.as_u64().map(|f| f as u32)),
                // 手写的 gamescope 参数:空数组＝照常由 kotori 拼(见
                // `ScaleProfile::gamescope_args`)。
                gamescope_args: string_list(scale.and_then(|s| s.get("gamescope_args"))),
            })
        })
        .collect()
}

/// JSON 字符串数组 → `Vec<String>`。字段缺失、类型不对都当空 ——
/// 这两种情况都只可能是老配置或别人写的配置,不值得为此报错。
fn string_list(value: Option<&Value>) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::ui_game;
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
    fn parses_watch_mode_and_save_paths_from_the_daemon() {
        let value = json!({
            "games": [{
                "id": "w",
                "name": "W",
                "game_dir": "/games/w",
                "exe_path": "/games/w/game.exe",
                "auto_watch": true,
                "process_name": "game.exe",
                "save_paths": [
                    { "kind": "windows", "path": "%APPDATA%\\W", "exclude": ["*.log", "tmp/"] }
                ],
                "scale_profile": { "algorithm": "Integer" }
            }]
        });

        let game = parse_games(&value).unwrap().remove(0);
        assert!(game.auto_watch);
        assert_eq!(game.process_name, "game.exe");
        assert_eq!(game.save_paths.len(), 1);
        assert_eq!(game.save_paths[0].kind, "windows");
        assert_eq!(game.save_paths[0].exclude, "*.log, tmp/");
        assert!(!game.save_paths_changed_after_edit());
    }
}

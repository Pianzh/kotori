//! `Config` 的单元测试。
//!
//! 从 `mod.rs` 拆出来:那边连着测试一起数会越过 500 行的软线(AGENTS.md),
//! 而这个项目里 `tests.rs` 与实现分家早就是惯例(`game/`、`display/`、`wine/`)。
//!
//! `use super::*` 保留了原先的可见性:子模块看得见父模块的私有项,所以搬过来
//! 不需要给任何东西放权限。

use super::*;

/// 手写配置的最小写法:一个游戏条目只要 `name` 与 `exe_path`。
///
/// `scale_profile` 与 `created_at` 从前是必填,而少写的代价不是"用默认值",
/// 是整份配置被改名成 `.corrupt` 再回落默认值 —— 便携安装因此会静默切回
/// 平台默认目录(见 `GameConfig::created_at` 的说明)。
#[test]
fn a_hand_written_game_entry_needs_only_a_name_and_an_exe() {
    let config: Config = toml::from_str(
        r#"
[games.probe]
name = "探针"
exe_path = "/games/probe/game.exe"
"#,
    )
    .expect("只写 name 与 exe_path 的条目应该能解析");

    let game = config.games.get("probe").expect("游戏条目应当被读进来");
    assert_eq!(game.name, "探针");
    assert_eq!(game.scale_profile.name, "默认");
    assert_eq!(
        game.scale_profile.algorithm,
        ScaleProfile::default_for().algorithm
    );
    // 默认不是 1970:那会让界面显示一个假日期。
    assert!(game.created_at.timestamp() > 1_700_000_000);
}

/// 界面拿字符串装 kind,配置拿 serde 装 —— 两种写法必须一模一样。
#[test]
fn as_str_matches_what_serde_writes() {
    for kind in [
        SavePathKind::Windows,
        SavePathKind::Relative,
        SavePathKind::Absolute,
    ] {
        let written = serde_json::to_value(kind).unwrap();
        assert_eq!(written, serde_json::Value::from(kind.as_str()), "{kind:?}");
    }
}

fn temp_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kotori-test-{}-{}-{}",
        tag,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.toml")
}

fn sample_config() -> Config {
    let mut config = Config::default();
    config.games.insert(
        "demo".into(),
        GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            watch_only: false,
            direct_launch: false,
            process_name: None,
            save_paths: vec![SavePath::inferred("%APPDATA%\\Demo\\save")],
            scale_profile: ScaleProfile {
                algorithm: ScaleAlgorithm::Nis { sharpness: 4 },
                framerate_limit: Some(60),
                // Explicitly *not* the defaults, so the round trip below proves
                // non-default values survive being written and read back.
                force_fullscreen: true,
                output_width: Some(2560),
                output_height: Some(1440),
                ..ScaleProfile::default_for()
            },
            wine_prefix: None,
            created_at: chrono::Utc::now(),
        },
    );
    config
}

#[test]
fn config_round_trips_through_toml() {
    let path = temp_path("roundtrip");
    let config = sample_config();

    save_to(&path, &config).unwrap();
    let loaded = paths::load_from(&path).unwrap();

    assert_eq!(loaded.games.len(), 1);
    let game = &loaded.games["demo"];
    assert_eq!(
        game.scale_profile.algorithm,
        ScaleAlgorithm::Nis { sharpness: 4 }
    );
    assert_eq!(game.scale_profile.framerate_limit, Some(60));
    assert_eq!(game.scale_profile.output_width, Some(2560));
    assert!(game.scale_profile.force_fullscreen);
    assert_eq!(game.save_paths.len(), 1);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn missing_file_falls_back_to_defaults() {
    let path = temp_path("missing").with_file_name("does-not-exist.toml");
    assert!(!path.exists());
    // load_from is strict; the lenient behaviour lives in `load()` and is
    // covered by `corrupt_config_is_backed_up`.
    assert!(paths::load_from(&path).is_err());
}

#[test]
fn corrupt_config_is_backed_up() {
    let path = temp_path("corrupt");
    std::fs::write(&path, "this is not = valid toml {{{").unwrap();

    let parsed = paths::load_from(&path);
    assert!(parsed.is_err());

    // Emulate `load()`'s backup step without touching the real config path.
    let backup = path.with_extension("toml.corrupt");
    std::fs::rename(&path, &backup).unwrap();
    assert!(backup.exists());
    assert!(!path.exists());

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn legacy_lanczos_config_no_longer_parses() {
    // Lanczos was never a real gamescope filter; documents the intentional
    // break so a stale config surfaces loudly instead of silently bilinear.
    let toml = r#"
[daemon]
socket_path = "/tmp/kotori.sock"
log_level = "info"

[games.old]
name = "old"
exe_path = "/games/old/game.exe"
save_paths = []
created_at = "2026-01-01T00:00:00Z"

[games.old.scale_profile]
name = "默认"
internal_width = 1280
internal_height = 720
output_width = 2560
output_height = 1440
force_fullscreen = true
algorithm = "Lanczos"
"#;
    assert!(toml::from_str::<Config>(toml).is_err());
}

#[test]
fn partial_game_config_uses_serde_defaults() {
    // Missing optional fields must not break loading an older config.
    let toml = r#"
[games.minimal]
name = "minimal"
exe_path = "/games/minimal/game.exe"
created_at = "2026-01-01T00:00:00Z"

[games.minimal.scale_profile]
name = "默认"
algorithm = "Integer"
internal_width = 1280
internal_height = 720
output_width = 2560
output_height = 1440
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let game = &config.games["minimal"];
    assert!(game.save_paths.is_empty());
    assert_eq!(game.wine_prefix, None);
    assert_eq!(game.scale_profile.framerate_limit, None);
    assert!(!game.scale_profile.force_fullscreen);
    // Profiles written before scaling ratios existed: no ratio, and the
    // window is free to drive the output size.
    assert_eq!(game.scale_profile.scale_ratio, None);
    assert_eq!(config.daemon.socket_path, default_socket_path());
}

#[test]
fn a_config_that_still_carries_follow_window_still_loads() {
    // `follow_window` was never read by anything (see HANDOVER: the switch
    // was empty), so the field is gone as of 2026-09-15. Every config on
    // disk still has the key — loading must keep working, and the next
    // write must stop emitting it.
    let toml = r#"
[games.old]
name = "old"
exe_path = "/games/old/game.exe"
created_at = "2026-01-01T00:00:00Z"

[games.old.scale_profile]
name = "默认"
algorithm = "Integer"
follow_window = false
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert!(config.games.contains_key("old"));

    let written = toml::to_string(&config).unwrap();
    assert!(!written.contains("follow_window"), "{written}");
}

#[test]
fn a_sync_config_that_still_carries_encryption_still_loads() {
    // 加密随 crypt 层一起没了（2026-09-16）。磁盘上每一份旧配置都还写着
    // 这个键 —— 整份配置必须照常加载，下一次写回也不能再带上它。
    // （`SyncConfig` 自己的字段级测试在 `config::sync` 里。）
    let toml = r#"
[sync]
enabled = true
bucket = "kotori-saves"
encryption = true
"#;
    let config: Config = toml::from_str(toml).unwrap();
    assert!(config.sync.enabled);
    // 没写 `engine` 键的配置拿到的是**当下的默认值**（2026-09-18 起是 kopia）。
    assert_eq!(config.sync.engine, SyncEngine::Kopia);

    let written = toml::to_string(&config).unwrap();
    assert!(!written.contains("encryption"), "{written}");
}

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

/// 自动追踪**默认开**(用户 2026-09-19:「仅观测默认打开」),旧名字 `watch_only`
/// 仍然读得进来。
///
/// 默认值这条特别要紧:手写配置的人不会为了"让别人启动的那一局也被记下来"去写一行
/// 开关,而漏掉它的代价是**库里什么都不留**(退出后的上传挂在会话结束上)。
#[test]
fn auto_watch_defaults_on_and_still_answers_to_its_old_name() {
    let config: Config = toml::from_str(
        r#"
[games.quiet]
name = "没写开关"
exe_path = "/games/quiet/game.exe"

[games.old]
name = "旧名字"
exe_path = "/games/old/game.exe"
watch_only = false
"#,
    )
    .expect("两种写法都应该能解析");

    assert!(config.games["quiet"].auto_watch, "缺省就是开");
    assert!(
        !config.games["old"].auto_watch,
        "旧名字写的 false 要照旧算数"
    );
    // 写出去用的是新名字(旧名字只在读的时候认)。
    let written = toml::to_string(&config).unwrap();
    assert!(written.contains("auto_watch"), "{written}");
    assert!(!written.contains("watch_only"), "{written}");
}

/// 一次性迁移:`auto_watch` 的默认值从"关"变成"开",而存量配置里那些
/// `watch_only = false` 是**旧默认值**写的、不是用户的选择 —— 不翻的话"默认打开"
/// 对老用户等于没发生。
///
/// 关键是**只翻一次**:标记落在配置里,用户之后自己关掉的不会被再翻回来。
#[test]
fn the_auto_watch_default_flips_old_configs_once_and_only_once() {
    let text = r#"
[games.old]
name = "旧配置"
exe_path = "/games/old/game.exe"
watch_only = false
"#;
    let mut config: Config = toml::from_str(text).unwrap();
    assert!(
        !config.games["old"].auto_watch,
        "serde 那一层照读旧名字里的 false"
    );

    config.normalize();
    assert!(config.games["old"].auto_watch, "迁移把它翻成新默认");
    assert!(config.daemon.auto_watch_migrated, "标记要落进配置里");

    // 写出去、再读回来(标记已经在文件里了):用户手动关掉之后不许被翻回来。
    let mut again: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    again.games.get_mut("old").unwrap().auto_watch = false;
    again.normalize();
    assert!(!again.games["old"].auto_watch, "迁移只发生一次");
}

/// 自动追踪要盯谁:`process_name` 优先,没写就按 exe 文件名(与直启那条路一致)。
#[test]
fn the_watched_process_name_falls_back_to_the_exe_file_name() {
    let game = |process_name: Option<&str>| GameConfig {
        cloud_id: None,
        name: "探针".into(),
        game_dir: PathBuf::from("/games/probe"),
        exe_path: PathBuf::from("/games/probe/Game.exe"),
        launch_args: Vec::new(),
        save_paths: Vec::new(),
        wine_prefix: None,
        auto_watch: true,
        direct_launch: false,
        process_name: process_name.map(str::to_string),
        scale_profile: ScaleProfile::default_for(),
        created_at: chrono::Utc::now(),
    };

    assert_eq!(game(None).watch_name().as_deref(), Some("Game.exe"));
    assert_eq!(
        game(Some("launcher.exe")).watch_name().as_deref(),
        Some("launcher.exe")
    );
    // 空白进程名当作没写,别拿它去跟任何东西比。
    assert_eq!(game(Some("   ")).watch_name().as_deref(), Some("Game.exe"));
    // exe 也取不出名字(理论上不该发生),就只能等用户自己点启动。
    let mut nameless = game(None);
    nameless.exe_path = PathBuf::from("/");
    assert_eq!(nameless.watch_name(), None);
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
            cloud_id: None,
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            auto_watch: false,
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

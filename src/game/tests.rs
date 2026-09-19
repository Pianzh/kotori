//! `game` 的单测：exe 挑选的评分与 `scan` 的层级判定。
//!
//! 全部用临时目录自造夹具，不依赖外部环境与执行顺序 —— CI 上要能一眼看出对错。

use std::path::PathBuf;

use super::*;

/// Self-cleaning scratch directory under the system temp dir.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-game-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn with(&self, files: &[&str]) -> &Self {
        for file in files {
            std::fs::write(self.0.join(file), b"").unwrap();
        }
        self
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn file_name(picked: &Path) -> String {
    picked.file_name().unwrap().to_string_lossy().to_string()
}

#[test]
fn prefers_the_chinese_localised_executable() {
    let dir = TempDir::new("chs");
    dir.with(&["game.exe", "game.chs.exe", "readme.txt"]);
    let picked = pick_game_exe(&dir.path()).unwrap();
    assert_eq!(file_name(&picked), "game.chs.exe");
}

#[test]
fn skips_installers_and_tools() {
    let dir = TempDir::new("helpers");
    dir.with(&[
        "setup.exe",
        "uninstall.exe",
        "sigluscounter.exe",
        "注册表恢复.exe",
        "ADVcore.exe",
    ]);
    let picked = pick_game_exe(&dir.path()).unwrap();
    assert_eq!(file_name(&picked), "ADVcore.exe");
}

#[test]
fn falls_back_to_an_exe_when_only_helpers_exist() {
    // Documents the current last-resort behaviour (e.g. dosbox.exe games).
    let dir = TempDir::new("only-helpers");
    dir.with(&["dosbox.exe"]);
    let picked = pick_game_exe(&dir.path()).unwrap();
    assert_eq!(file_name(&picked), "dosbox.exe");
}

#[test]
fn no_executables_means_no_candidate() {
    let dir = TempDir::new("no-exe");
    dir.with(&["readme.txt", "data.pak"]);
    assert!(pick_game_exe(&dir.path()).is_none());
}

#[test]
fn game_ids_are_stable_and_filesystem_safe() {
    assert_eq!(generate_game_id("My Game v0.99"), "my-game-v0-99");
    assert_eq!(generate_game_id("测试游戏(正式版)"), "测试游戏-正式版");
    assert_eq!(generate_game_id("My-Game"), "my-game");
    assert_eq!(generate_game_id("  "), "");
    // Same directory name must always produce the same id.
    assert_eq!(generate_game_id("SomeGame"), generate_game_id("SomeGame"));
}

#[test]
fn a_second_entry_with_the_same_name_gets_a_numeric_suffix() {
    // Two library entries for one game are legal (two launch sets, two save
    // sets, one launching and one watch-only) — the id collides, so the second
    // one gets `-2` instead of the add being refused. A UUID would fix the
    // collision by making the id unreadable, and the id shows up in the cloud
    // layout.
    let mut config = crate::config::Config::default();
    let first = generate_unique_game_id(&config, "3days");
    config.games.insert(first, game_config_named("3days"));

    assert_eq!(generate_unique_game_id(&config, "3days"), "3days-2");
    // ...and the third one skips past the second.
    config
        .games
        .insert("3days-2".to_string(), game_config_named("3days"));
    assert_eq!(generate_unique_game_id(&config, "3days"), "3days-3");
    // A different name is unaffected by the collisions.
    assert_eq!(generate_unique_game_id(&config, "narcissu"), "narcissu");
}

#[test]
fn an_exe_already_used_by_another_entry_warns_but_stays_allowed() {
    let dir = TempDir::new("duplicate-exe");
    dir.with(&["game.exe"]);
    let exe = dir.path().join("game.exe");

    let mut config = crate::config::Config::default();
    let mut game = game_config_named("原型");
    game.exe_path = exe.clone();
    config.games.insert("yuan-xing".to_string(), game);

    // Same file, different spelling: canonicalization must still match it.
    let awkward = dir.path().join("./game.exe");
    let warning = duplicate_exe_warning(&config, &awkward, None).expect("same exe must warn");
    assert!(warning.contains("原型"), "warning names the other entry");
    assert!(
        warning.contains("允许"),
        "the warning must not imply a refusal"
    );

    // The entry itself is not a duplicate of itself.
    assert!(duplicate_exe_warning(&config, &exe, Some("yuan-xing")).is_none());
    // A different exe does not warn.
    dir.with(&["other.exe"]);
    assert!(duplicate_exe_warning(&config, &dir.path().join("other.exe"), None).is_none());
}

fn game_config_named(name: &str) -> crate::config::GameConfig {
    crate::config::GameConfig {
        name: name.to_string(),
        game_dir: PathBuf::from("/games/demo"),
        exe_path: PathBuf::from("/games/demo/game.exe"),
        launch_args: Vec::new(),
        save_paths: Vec::new(),
        wine_prefix: None,
        watch_only: false,
        process_name: None,
        scale_profile: crate::config::ScaleProfile::default_for(),
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn scan_counts_subdirectories_and_the_root_itself() {
    let root = TempDir::new("scan-root");
    std::fs::create_dir_all(root.path().join("GameA")).unwrap();
    std::fs::write(root.path().join("GameA").join("game.chs.exe"), b"").unwrap();
    std::fs::create_dir_all(root.path().join("EmptyGame")).unwrap();
    std::fs::write(root.path().join("loose.exe"), b"").unwrap();

    let found = scan(&root.path()).unwrap();
    assert_eq!(found.len(), 2);
    let game_a = found.iter().find(|g| g.name == "GameA").unwrap();
    assert_eq!(file_name(&game_a.exe_path), "game.chs.exe");
    // The loose exe belongs to the scanned directory itself, not to the empty
    // subdirectory next to it.
    let root_entry = found
        .iter()
        .find(|g| file_name(&g.exe_path) == "loose.exe")
        .unwrap();
    assert_eq!(
        root_entry.name,
        root.path().file_name().unwrap().to_string_lossy()
    );
    // 扫描**不再**把某台机器的分辨率写进档案(2026-09-13):窗口尺寸留空,
    // 启动时按游戏实际落在的那块屏算(见 `ScaleProfile::output_size_for`)。
    // 这样换显示器、换机器都不用重新扫描。
    assert_eq!(game_a.scale_profile.output_width, None);
    assert_eq!(game_a.scale_profile.output_height, None);
    assert_eq!(game_a.scale_profile.explicit_output_size(), None);
    // 游戏分辨率同理:没人探测过它,所以不写(gamescope 自己会按 1280x720 画)。
    assert_eq!(game_a.scale_profile.explicit_internal_size(), None);
}

#[test]
fn scan_counts_the_scanned_directory_itself() {
    // An exe at the root of the *scanned* directory used to be invisible to
    // `scan <dir>` (it only looked at subdirectories), while scanning the
    // parent found the very same exe.
    let dir = TempDir::new("self");
    dir.with(&["Game.exe", "readme.txt"]);

    let found = scan(&dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(file_name(&found[0].exe_path), "Game.exe");
    assert_eq!(
        found[0].name,
        dir.path().file_name().unwrap().to_string_lossy()
    );
    assert_eq!(found[0].game_dir, dir.path());
}

#[test]
fn self_and_subdirectory_hits_stay_two_distinct_entries() {
    // A self-hit and a subdirectory hit are two different game directories, so
    // both appear — but no directory may ever contribute more than one entry.
    let root = TempDir::new("dup");
    root.with(&["Game.exe"]);
    std::fs::create_dir_all(root.path().join("Bundle")).unwrap();
    std::fs::write(root.path().join("Bundle").join("bundle.exe"), b"").unwrap();

    let found = scan(&root.path()).unwrap();
    assert_eq!(found.len(), 2);
    let mut exes: Vec<String> = found.iter().map(|g| file_name(&g.exe_path)).collect();
    exes.sort();
    assert_eq!(exes, vec!["Game.exe".to_string(), "bundle.exe".to_string()]);
    let dirs: Vec<PathBuf> = found.iter().map(|g| g.game_dir.clone()).collect();
    assert_ne!(dirs[0], dirs[1]);
}

#[test]
fn scan_of_missing_directory_is_an_error() {
    // A typo in the path must not be indistinguishable from an empty
    // directory: the CLI has to exit non-zero.
    let missing = std::env::temp_dir().join("kotori-does-not-exist-xyz");
    assert!(scan(&missing).is_err());
}

#[test]
fn scan_of_a_file_is_an_error_too() {
    // "Exists but is a file" fails in read_dir; both invalid shapes end in
    // Err instead of a silent empty result.
    let dir = TempDir::new("not-a-dir");
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"").unwrap();
    assert!(scan(&file).is_err());
}

#[test]
fn an_uninstaller_is_not_the_game_even_when_it_is_called_uninstaller() {
    // Measured on the real library: `rance3/` had picked `Uninstaller.exe`,
    // because the helper list only knew the bare word "uninstall" (so
    // "uninstaller" was not filtered) and ties fell to directory order.
    let dir = TempDir::new("rance3");
    dir.with(&[
        "OpenSaveFolder.exe",
        "Rance03.exe",
        "ResetConfig.exe",
        "Uninstaller.exe",
    ]);
    let picked = pick_game_exe(&dir.path()).unwrap();
    assert_eq!(file_name(&picked), "Rance03.exe");
}

#[test]
fn the_executable_named_like_its_directory_beats_a_lookalike() {
    // A localised build is usually the one that carries the directory's name.
    let dir = TempDir::new("affinity");
    let game = dir.path().join("SiglusEngineCHS");
    std::fs::create_dir_all(&game).unwrap();
    for file in [
        "SiglusEngine.exe",
        "SiglusEngineCHS.exe",
        "SiglusCounter.exe",
    ] {
        std::fs::write(game.join(file), b"").unwrap();
    }
    let picked = pick_game_exe(&game).unwrap();
    assert_eq!(file_name(&picked), "SiglusEngineCHS.exe");
}

#[test]
fn helpers_are_recognised_by_prefix_too() {
    for stem in [
        "uninstaller",
        "unins000",
        "setup_x",
        "installer",
        "configtool",
        "autoupdatecheck",
    ] {
        assert!(is_helper(stem), "{stem} 应该被当作工具");
    }
    for stem in ["rance03", "advcore", "siglusenginechs", "game"] {
        assert!(!is_helper(stem), "{stem} 是游戏本体，不该被过滤");
    }
}

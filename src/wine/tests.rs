//! `mod.rs` 那一半的测试:前缀怎么找、wineserver 怎么收尾。
//!
//! 存档路径的测试在 `path_tests.rs`。

use super::test_support::*;
use super::*;

#[test]
fn portable_prefix_inside_the_game_dir_wins_over_the_default() {
    let dir = std::env::temp_dir().join(format!("kotori-portable-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("drive_c/users/tester")).unwrap();

    let config = Config::default();
    let game = game(
        &dir.to_string_lossy(),
        &format!("{}/game.exe", dir.display()),
    );
    let (prefix, source) = resolve_prefix(&game, &config);

    assert_eq!(prefix, dir);
    assert!(matches!(source, PrefixSource::Portable(_)), "{source:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn per_game_prefix_beats_the_global_one() {
    let mut config = Config::default();
    config.wine.prefix = Some(PathBuf::from("/global/prefix"));

    let mut with_own = game("/games/demo", "/games/demo/game.exe");
    with_own.wine_prefix = Some(PathBuf::from("/game/prefix"));
    assert_eq!(
        resolve_prefix(&with_own, &config),
        (PathBuf::from("/game/prefix"), PrefixSource::Game)
    );

    let inherited = game("/games/demo", "/games/demo/game.exe");
    assert_eq!(
        resolve_prefix(&inherited, &config),
        (PathBuf::from("/global/prefix"), PrefixSource::Global)
    );
}

#[test]
fn falls_back_to_the_default_prefix() {
    let config = Config::default();
    let game = game("/nonexistent/game", "/nonexistent/game/game.exe");
    // No WINEPREFIX, no portable prefix, nothing detected.
    let (_, source) = resolve_prefix_with(&game, &config, None);
    assert!(
        matches!(source, PrefixSource::Default | PrefixSource::Detected(_)),
        "{source:?}"
    );
}

#[test]
fn the_environment_prefix_is_used_when_nothing_else_is() {
    let config = Config::default();
    let game = game("/nonexistent/game", "/nonexistent/game/game.exe");
    let (prefix, source) = resolve_prefix_with(&game, &config, Some(PathBuf::from("/env/prefix")));
    assert_eq!(prefix, PathBuf::from("/env/prefix"));
    assert_eq!(source, PrefixSource::Environment);

    // An empty value is ignored rather than becoming an empty prefix.
    let (_, source) = resolve_prefix_with(&game, &config, Some(PathBuf::from("")));
    assert_ne!(source, PrefixSource::Environment);
}

/// A scratch directory that names itself, so parallel tests never share one.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kotori-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake `wineserver` that writes down what it was asked to do.
fn recording_wineserver(dir: &Path) -> PathBuf {
    let script = dir.join("wineserver");
    let log = dir.join("call.txt");
    crate::secrets::testing::write_executable(
        &script,
        &format!(
            "#!/bin/sh\n\
                 if [ \"$1\" = \"--kotori-warmup\" ]; then exit 0; fi\n\
                 echo \"args=$* prefix=$WINEPREFIX\" > {}\n",
            log.display()
        ),
    );
    script
}

#[tokio::test]
async fn closing_a_prefix_names_that_prefix_to_wineserver() {
    let dir = scratch("wineserver-args");
    let script = recording_wineserver(&dir);
    let prefix = dir.join("prefix");

    close_prefix_with(&script, &prefix).await;

    let recorded = std::fs::read_to_string(dir.join("call.txt")).unwrap();
    assert!(recorded.contains("args=-k"), "{recorded}");
    assert!(
        recorded.contains(&prefix.display().to_string()),
        "the prefix must be the one we were asked to close: {recorded}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_missing_wineserver_is_survivable() {
    // This runs while a session is being torn down, so "wine is not where we
    // thought it was" must not turn into a failed teardown or a panic.
    let missing = scratch("wineserver-missing").join("nope");
    close_prefix_with(&missing, Path::new("/games/demo/prefix")).await;
}

#[tokio::test]
async fn a_wineserver_that_never_answers_does_not_hold_up_the_teardown() {
    // The whole point of the timeout: a wedged helper must not become a
    // wedged shutdown of its own.
    let dir = scratch("wineserver-hang");
    let script = dir.join("wineserver");
    crate::secrets::testing::write_executable(
        &script,
        "#!/bin/sh\n\
             if [ \"$1\" = \"--kotori-warmup\" ]; then exit 0; fi\n\
             exec sleep 60\n",
    );

    let started = std::time::Instant::now();
    close_prefix_with(&script, Path::new("/games/demo/prefix")).await;

    let waited = started.elapsed();
    assert!(
        waited < WINESERVER_KILL_TIMEOUT * 3,
        "waited {waited:?}, which means the timeout did not fire"
    );
    std::fs::remove_dir_all(&dir).ok();
}

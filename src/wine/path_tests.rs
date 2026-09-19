//! 存档路径那一半的测试:`SaveRoot` / 令牌 / 「浏览…」挑回来的路径怎么落进档案。
//!
//! 与 `tests.rs` 分开只是因为行数(两个文件合起来 500 行开外),不是因为它们是两件事。

use super::test_support::*;
use super::*;
use crate::config::{GameConfig, SavePath, SavePathKind};

#[test]
fn infers_the_storage_kind_from_a_bare_path() {
    assert_eq!(
        SavePathKind::infer("%APPDATA%\\X\\save"),
        SavePathKind::Windows
    );
    assert_eq!(
        SavePathKind::infer("C:\\users\\a\\save"),
        SavePathKind::Windows
    );
    assert_eq!(SavePathKind::infer("d:/save"), SavePathKind::Windows);
    assert_eq!(SavePathKind::infer("savedata"), SavePathKind::Relative);
    assert_eq!(SavePathKind::infer("save/"), SavePathKind::Relative);
    assert_eq!(SavePathKind::infer("/opt/saves/x"), SavePathKind::Absolute);
    assert_eq!(SavePathKind::infer("~/saves/x"), SavePathKind::Absolute);
}

#[test]
fn resolves_tokens_inside_the_prefix() {
    let prefix = FakePrefix::new("tokens", &["tester"]);
    let base = prefix.path().join("drive_c/users/tester");

    for (input, expected) in [
        (
            "%APPDATA%\\Game\\save",
            base.join("AppData/Roaming/Game/save"),
        ),
        ("%LOCALAPPDATA%\\Game", base.join("AppData/Local/Game")),
        ("%DOCUMENTS%\\Game", base.join("Documents/Game")),
        ("%SAVEDGAMES%\\Game", base.join("Saved Games/Game")),
        ("%USERPROFILE%\\Game", base.join("Game")),
    ] {
        let save = SavePath::inferred(input);
        assert_eq!(
            resolve_save_path(
                &SaveRoot::WinePrefix(prefix.path()),
                Path::new("/games/demo"),
                &save
            )
            .unwrap(),
            expected,
            "for {input}"
        );
    }
}

#[test]
fn resolves_literal_drive_paths_and_mixed_separators() {
    let prefix = FakePrefix::new("drive", &["tester"]);
    let saved = resolve_save_path(
        &SaveRoot::WinePrefix(prefix.path()),
        Path::new("/games/demo"),
        &SavePath::new(SavePathKind::Windows, "C:/users/tester/Documents/Game"),
    )
    .unwrap();
    assert_eq!(
        saved,
        prefix.path().join("drive_c/users/tester/Documents/Game")
    );
}

#[test]
fn unknown_tokens_are_reported_not_guessed() {
    let prefix = FakePrefix::new("bad-token", &["tester"]);
    let err = resolve_save_path(
        &SaveRoot::WinePrefix(prefix.path()),
        Path::new("/games/demo"),
        &SavePath::new(SavePathKind::Windows, "%PROGRAMFILES%\\Game"),
    )
    .unwrap_err();
    assert!(err.contains("%PROGRAMFILES%"), "{err}");
    assert!(
        err.contains("%APPDATA%"),
        "the message should list the tokens"
    );
}

/// 「浏览…」挑回来的本机路径 → 档案里的写法:两个方向必须能对上(挑完存进去,
/// 下次启动时再解析出来,得回到同一个地方)。
#[test]
fn a_picked_path_turns_into_the_written_form_and_back() {
    // relative:只在游戏目录里面才成立,出来就明说。
    assert_eq!(
        to_relative_path(
            Path::new("/games/demo"),
            Path::new("/games/demo/savedata/x")
        )
        .unwrap(),
        "savedata/x"
    );
    let outside =
        to_relative_path(Path::new("/games/demo"), Path::new("/elsewhere/save")).unwrap_err();
    assert!(outside.contains("absolute"), "{outside}");
    assert!(to_relative_path(Path::new("/games/demo"), Path::new("/games/demo")).is_err());

    let prefix = FakePrefix::new("reverse", &["tester"]);
    let user = prefix.path().join("drive_c/users/tester");
    for (picked, written) in [
        (
            user.join("AppData/Roaming/Game/save"),
            "%APPDATA%\\Game\\save",
        ),
        // 长前缀优先:AppData/Local 不能被更短的规则吃掉。
        (user.join("AppData/Local/Game"), "%LOCALAPPDATA%\\Game"),
        (user.join("Documents/Game"), "%DOCUMENTS%\\Game"),
        (user.join("Saved Games/Game"), "%SAVEDGAMES%\\Game"),
        // 用户目录里的其它地方(桌面、下载……)用 %USERPROFILE% 兜底,照样跨系统。
        (user.join("Desktop/Game"), "%USERPROFILE%\\Desktop\\Game"),
        (user.clone(), "%USERPROFILE%"),
    ] {
        assert_eq!(
            to_windows_token(&picked).unwrap(),
            written,
            "for {picked:?}"
        );

        // 反过来解析:回到挑出来的那个位置。
        let back = resolve_save_path(
            &SaveRoot::WinePrefix(prefix.path()),
            Path::new("/games/demo"),
            &SavePath::new(SavePathKind::Windows, written),
        )
        .unwrap();
        assert_eq!(back, picked, "for {written}");
    }
}

/// 不在任何 prefix 里的路径写不成令牌 —— 说清楚,别编一个 `C:\...` 出来
/// (那种东西在真 Windows 上必然解析失败,ADR-008)。
#[test]
fn a_path_outside_a_prefix_is_refused_with_a_way_out() {
    let err = to_windows_token(Path::new("/opt/saves/demo")).unwrap_err();
    assert!(err.contains("drive_c"), "{err}");
    assert!(err.contains("absolute"), "{err}");
}

/// 真 Windows 上从资源管理器挑出来的路径(`C:\Users\<我>\AppData\Roaming\…`)
/// 也要写得出令牌。
///
/// 这条测试**在 Linux 上跑**,而它验的正是 Windows 才会出现的形状 —— 能做到这点
/// 是因为 `to_windows_token` 切的是字符而不是 `Path::components()`(`\` 在 Linux
/// 上不是分隔符)。在那之前这个形状必然翻译失败,于是真机上「浏览…」点了没反应。
#[test]
fn a_real_windows_profile_path_becomes_a_token_too() {
    for (picked, written) in [
        (
            r"C:\Users\tester\AppData\Roaming\Game\save",
            r"%APPDATA%\Game\save",
        ),
        (
            r"C:\Users\tester\AppData\Local\Game",
            r"%LOCALAPPDATA%\Game",
        ),
        (r"C:\Users\tester\Documents\Game", r"%DOCUMENTS%\Game"),
        (r"C:\Users\tester\Saved Games\Game", r"%SAVEDGAMES%\Game"),
        (
            r"C:\Users\tester\Desktop\Game",
            r"%USERPROFILE%\Desktop\Game",
        ),
        (r"C:\Users\tester", r"%USERPROFILE%"),
        // 大小写不敏感:Windows 上这两段的写法不固定。
        (r"C:\users\Tester\documents\Game", r"%DOCUMENTS%\Game"),
        // 正斜杠也吃(配置里两种写法都出现过)。
        ("C:/Users/tester/AppData/Roaming/Game", r"%APPDATA%\Game"),
    ] {
        assert_eq!(
            to_windows_token(Path::new(picked)).unwrap(),
            written,
            "for {picked}"
        );
    }

    // 盘符有,但不在用户目录里 —— 照样要拒,并且给出路。
    let err = to_windows_token(Path::new(r"D:\Saves\Game")).unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

/// 「浏览…」挑完之后翻译的三档顺序:**相对 → 令牌 → 绝对**(用户 2026-09-18 定的)。
///
/// 这条顺序是"点浏览没反应"那个 bug 的正解:挑回来的路径自己决定用哪一档,而不是
/// 听用户在下拉框里预先选的那个 —— 选错了就翻译不成,而"翻译不成"在界面上等于
/// "什么都没发生"。
#[test]
fn a_picked_path_is_expressed_the_most_portable_way_first() {
    let game_dir = Path::new("/games/demo");
    let prefix = FakePrefix::new("portable", &["tester"]);
    let user = prefix.path().join("drive_c/users/tester");

    for (picked, kind, written) in [
        // ① 在游戏目录里面 → 相对:换台机器照样对得上,所以最优先。
        (
            PathBuf::from("/games/demo/savedata"),
            SavePathKind::Relative,
            "savedata",
        ),
        // ② 在用户目录里 → 令牌(wine 的 prefix 形状)。
        (
            user.join("AppData/Roaming/Game"),
            SavePathKind::Windows,
            r"%APPDATA%\Game",
        ),
        // ② 真 Windows 的形状也走令牌(以前这一条会掉到第三档,见上面的测试)。
        (
            PathBuf::from(r"C:\Users\tester\Documents\Game"),
            SavePathKind::Windows,
            r"%DOCUMENTS%\Game",
        ),
        // ③ 都不是 → 绝对,并如实标成"仅本机"。
        (
            PathBuf::from("/opt/saves/demo"),
            SavePathKind::Absolute,
            "/opt/saves/demo",
        ),
        (
            PathBuf::from(r"D:\Saves\Game"),
            SavePathKind::Absolute,
            r"D:\Saves\Game",
        ),
    ] {
        let (got_kind, got) = portable_save_path(game_dir, &picked);
        assert_eq!(got_kind, kind, "kind for {picked:?}");
        assert_eq!(got, written, "text for {picked:?}");
    }
}

#[test]
fn relative_paths_resolve_against_the_game_root() {
    let save = SavePath::new(SavePathKind::Relative, "savedata");
    assert_eq!(
        resolve_save_path(
            &SaveRoot::WinePrefix("/prefix".into()),
            Path::new("/games/demo"),
            &save
        )
        .unwrap(),
        PathBuf::from("/games/demo/savedata")
    );
}

#[test]
fn absolute_paths_stay_put_and_expand_home() {
    let save = SavePath::new(SavePathKind::Absolute, "/opt/saves/demo");
    assert_eq!(
        resolve_save_path(
            &SaveRoot::WinePrefix("/prefix".into()),
            Path::new("/games/demo"),
            &save
        )
        .unwrap(),
        PathBuf::from("/opt/saves/demo")
    );

    let home = SavePath::new(SavePathKind::Absolute, "~/saves/demo");
    let resolved = resolve_save_path(
        &SaveRoot::WinePrefix("/prefix".into()),
        Path::new("/games/demo"),
        &home,
    )
    .unwrap();
    assert!(resolved.ends_with("saves/demo"));
    assert!(!resolved.to_string_lossy().starts_with('~'));
}

#[test]
fn the_same_token_resolves_on_both_sides_of_a_dual_boot() {
    // One configuration, two systems: inside a wine prefix the token lands
    // under drive_c, on real Windows under the user profile. This equality
    // of *meaning* is what makes a save location portable.
    let prefix = FakePrefix::new("dual", &["tester"]);
    let save = SavePath::new(SavePathKind::Windows, "%APPDATA%\\Game\\save");

    let on_linux = resolve_save_path(
        &SaveRoot::WinePrefix(prefix.path()),
        Path::new("/games/demo"),
        &save,
    )
    .unwrap();
    assert_eq!(
        on_linux,
        prefix
            .path()
            .join("drive_c/users/tester/AppData/Roaming/Game/save")
    );

    let on_windows = resolve_save_path(
        &SaveRoot::UserProfile("C:/Users/tester".into()),
        Path::new("C:/games/demo"),
        &save,
    )
    .unwrap();
    assert_eq!(
        on_windows,
        PathBuf::from("C:/Users/tester/AppData/Roaming/Game/save")
    );

    // Both describe the same place, which is the whole point.
    assert_eq!(
        on_linux
            .strip_prefix(prefix.path().join("drive_c/users/tester"))
            .unwrap(),
        on_windows.strip_prefix("C:/Users/tester").unwrap()
    );
}

#[test]
fn windows_only_accepts_portable_drive_paths() {
    let save = SavePath::new(SavePathKind::Windows, "C:\\users\\tester\\Documents\\Game");
    // Inside a prefix any drive path is fine (it is one machine's prefix).
    let in_prefix = resolve_save_path(
        &SaveRoot::WinePrefix("/prefix".into()),
        Path::new("/games/demo"),
        &save,
    )
    .unwrap();
    assert_eq!(
        in_prefix,
        PathBuf::from("/prefix/drive_c/users/tester/Documents/Game")
    );

    // On real Windows a path outside the profile is rejected: it would be a
    // machine-specific absolute path pretending to be portable.
    let on_windows = resolve_save_path(
        &SaveRoot::UserProfile("C:/Users/tester".into()),
        Path::new("C:/games/demo"),
        &save,
    )
    .unwrap();
    assert_eq!(on_windows, PathBuf::from("C:/Users/tester/Documents/Game"));

    let bad = SavePath::new(SavePathKind::Windows, "C:\\Program Files\\Game\\save");
    let error = resolve_save_path(
        &SaveRoot::UserProfile("C:/Users/tester".into()),
        Path::new("C:/games/demo"),
        &bad,
    )
    .unwrap_err();
    assert!(error.contains("令牌"), "{error}");
}

#[test]
fn picks_the_current_user_directory_first() {
    let prefix = FakePrefix::new("users", &["steamuser", "Public"]);
    let user_dir = windows_user_dir(&prefix.path());
    // `steamuser` is the only real account here, `Public` is never a game's home.
    assert!(user_dir.ends_with("users/steamuser"), "{user_dir:?}");
}

#[test]
fn save_path_accepts_both_config_forms() {
    #[derive(serde::Deserialize)]
    struct Holder {
        save_paths: Vec<SavePath>,
    }

    let toml = r#"
save_paths = [
  "savedata",
  "%APPDATA%\\Game\\save",
  { kind = "absolute", path = "/opt/saves/demo", exclude = ["*.log"] },
]
"#;
    let holder: Holder = toml::from_str(toml).unwrap();
    assert_eq!(holder.save_paths[0].kind, SavePathKind::Relative);
    assert_eq!(holder.save_paths[1].kind, SavePathKind::Windows);
    assert_eq!(holder.save_paths[2].kind, SavePathKind::Absolute);
    assert_eq!(holder.save_paths[2].exclude, ["*.log"]);

    // Round-trips back out as explicit tables.
    let mut config = Config::default();
    config.games.insert(
        "demo".into(),
        GameConfig {
            save_paths: holder.save_paths,
            ..game("/games/demo", "/games/demo/game.exe")
        },
    );
    let text = toml::to_string_pretty(&config).unwrap();
    assert!(text.contains("kind = \"windows\""), "{text}");
    let reparsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(reparsed.games["demo"].save_paths[2].exclude, ["*.log"]);
}

#[test]
fn typed_text_infers_its_kind_like_a_picked_path_would() {
    // 手动敲进输入框的文本也要自动认 kind(用户 2026-09-19),两条浏览遇不到的
    // 形态单独认:令牌写法本身,和裸相对写法。
    let game_dir = Path::new("/games/demo");

    // 令牌写法:原样保留,kind = windows(to_windows_token 认不出它)。
    let (kind, value) = infer_save_path(game_dir, r"%APPDATA%\Game\save").unwrap();
    assert_eq!(kind, SavePathKind::Windows);
    assert_eq!(value, r"%APPDATA%\Game\save");

    // 裸相对:原样,kind = relative。
    let (kind, value) = infer_save_path(game_dir, "savedata").unwrap();
    assert_eq!(kind, SavePathKind::Relative);
    assert_eq!(value, "savedata");

    // 用户目录形状的盘符路径 → 转成令牌(与浏览链路同一个"自动转化")。
    let (kind, value) = infer_save_path(game_dir, r"C:\Users\tester\AppData\Roaming\Game").unwrap();
    assert_eq!(kind, SavePathKind::Windows);
    assert_eq!(value, r"%APPDATA%\Game");

    // 游戏目录内的绝对路径 → 相对。
    let (kind, value) = infer_save_path(game_dir, "/games/demo/savedata").unwrap();
    assert_eq!(kind, SavePathKind::Relative);
    assert_eq!(value, "savedata");

    // 别的绝对路径 → absolute 原样。
    let (kind, value) = infer_save_path(game_dir, "/opt/saves/demo").unwrap();
    assert_eq!(kind, SavePathKind::Absolute);
    assert_eq!(value, "/opt/saves/demo");

    // 输入到一半的盘符:不改写、不猜(kind 保持原样)。
    assert!(infer_save_path(game_dir, "C:").is_none());
    // 空文本:没有可推断的东西。
    assert!(infer_save_path(game_dir, "  ").is_none());
}

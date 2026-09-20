//! `process` 的单元测试。
//!
//! 从 `mod.rs` 拆出来(那边连着测试一起数会越过 500 行,AGENTS.md),`tests.rs` 与
//! 实现分家在这个项目里是惯例(`config/`、`game/`、`wine/` 都这样)。
//!
//! `use super::*` 保留了原先的可见性:子模块看得见父模块的私有项,所以搬过来
//! 不需要给任何东西放权限。

use super::*;

/// 能挑的进程里必须有自己刚起的那个 `.exe` 模样的小东西 —— 而且**不能**有
/// wine 那层管道进程(`wineserver` 之类,用户挑了它毫无意义)。
#[cfg(unix)]
#[test]
fn the_pickable_list_keeps_game_shaped_processes_only() {
    // 目录名带上时间戳:上一次跑崩了留下的同名目录/探针别来搅这一轮。
    let dir = std::env::temp_dir().join(format!(
        "kotori-pickable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("kotori-pickable-probe.exe");
    // ⚠ **软链接**而不是 `fs::copy`:复制出来立刻 exec 会撞上 `ETXTBSY` ——
    // 别的测试线程在这两步之间 fork 出来的子进程继承了写句柄,内核就不让 exec
    // (e2e 的 `write_script` 为同一个坑写了重试,这里改用链接:那个 inode 是
    // 只读的 `/bin/sleep`,根本没有写句柄可继承)。命令行的 argv0 仍然是这个
    // `.exe` 路径,所以被测的那条路一点没变。
    std::os::unix::fs::symlink("/bin/sleep", &exe).unwrap();
    let mut child = std::process::Command::new(&exe)
        .arg("30")
        .spawn()
        .expect("spawn the probe");
    let pid = child.id() as i32;

    // 等它**两条都齐了**再断言:名字与命令行都是 `exec` 之后才稳定下来的。
    // 失败的报错把当时看到的候选一起打出来 —— 这条偶发红过一次(2026-09-20),
    // 没有现场就只能靠猜。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let found = loop {
        let list = pickable_processes();
        if let Some(entry) = list
            .iter()
            .find(|entry| entry.pid == pid && entry.exe.is_some())
        {
            break entry.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "刚起的进程没出现在可挑列表里;当时看到的候选: {list:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    child.kill().unwrap();
    child.wait().unwrap();

    assert_eq!(found.name, "kotori-pickable-probe.exe");
    // argv0 是绝对路径,所以 exe 能直接填进「添加游戏」。
    assert_eq!(found.exe.as_deref(), Some(exe.to_string_lossy().as_ref()));
    assert!(
        !pickable_processes()
            .iter()
            .any(|entry| entry.name == "wineserver"),
        "wine 的管道进程不该出现在候选里"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 命令行里那个 exe 换成本机路径:绝对路径原样、`Z:` 换根、相对路径靠 cwd 拼、
/// 别的盘符不猜(它在某个 prefix 里,而这里不知道是哪个)。
#[cfg(unix)]
#[test]
fn command_line_paths_become_unix_paths() {
    let path = |argv0: &str, cwd: Option<&str>| unix::unix_exe_path(argv0, cwd);
    assert_eq!(
        path(r"Z:\run\media\disk\Game\game.exe", None).as_deref(),
        Some("/run/media/disk/Game/game.exe")
    );
    assert_eq!(
        path("/games/demo/game.exe", None).as_deref(),
        Some("/games/demo/game.exe")
    );
    // 相对路径:进程自己的 cwd 说了算;读不到 cwd 就不猜。
    assert_eq!(
        path("target/probe/game.exe", Some("/home/user/kotori")).as_deref(),
        Some("/home/user/kotori/target/probe/game.exe")
    );
    assert_eq!(path("target/probe/game.exe", None), None);
    assert_eq!(path(r"C:\Games\demo\game.exe", None), None);
}

/// 自己这个进程当然活着,`/proc` 里不存在的号则不是 —— 两边的平台实现都只是
/// "查一下",所以这条在两个平台上都成立。
#[test]
fn pids_can_be_asked_who_is_alive() {
    assert!(pid_alive(std::process::id() as i32));
    assert!(!pid_alive(i32::MAX), "这么个号不该存在");
    assert!(!pid_alive(0), "0 不是进程");
}

#[test]
fn normalizes_windows_and_unix_paths() {
    assert_eq!(normalize_process_name("C:\\games\\x\\Game.exe"), "game.exe");
    assert_eq!(normalize_process_name("/usr/bin/wine"), "wine");
    assert_eq!(normalize_process_name("  game.exe  "), "game.exe");
    assert_eq!(normalize_process_name("\"game.exe\""), "game.exe");
    assert_eq!(normalize_process_name(""), "");
}

#[test]
fn matches_wine_style_processes() {
    // The game itself: wine rewrites argv[0] to a Windows path.
    assert!(matches(
        "game.exe",
        "game.exe",
        "C:\\games\\x\\game.exe\0--windowed\0"
    ));
    // Case is ignored, because Windows does not care either.
    assert!(matches("Game.EXE", "game.exe", "C:\\games\\x\\GAME.EXE\0"));
    // A native helper is matched through argv[0].
    assert!(matches("wine", "", "/usr/bin/wine\0game.exe\0"));
}

#[test]
fn matches_truncated_comm_names() {
    // `comm` holds at most 15 bytes, so a longer exe name arrives cut off:
    // `SiglusEngineCHS.exe` -> `SiglusEngineCHS`.
    assert_eq!(truncated("SiglusEngineCHS.exe"), "SiglusEngineCHS");
    assert!(matches("SiglusEngineCHS.exe", "SiglusEngineCHS", ""));

    // Even longer names still match through the truncated form.
    let long = "AVeryLongGameName.exe";
    let cut = truncated(long);
    assert_eq!(cut.chars().count(), 15);
    assert!(matches(long, &cut, ""));
}

#[test]
fn comm_truncation_counts_bytes_not_characters() {
    // `/proc/<pid>/comm` keeps 15 **bytes**, so a Chinese exe name arrives cut
    // in the middle: three bytes per character means five of them survive.
    assert_eq!(truncated("海猫鸣泣之时散语音版.exe"), "海猫鸣泣之");
    assert!(matches("海猫鸣泣之时散语音版.exe", "海猫鸣泣之", ""));
}

#[test]
fn plumbing_is_told_apart_from_a_game() {
    for name in [
        "gamescope",
        "gamescope-wl",
        "gamescopereaper",
        "Xwayland",
        "winedevice.exe",
        "services.exe",
        "/usr/bin/wine",
    ] {
        assert!(is_plumbing(name), "{name} 是管道进程，不能当成游戏");
    }
    for name in [
        "海猫鸣泣之时散语音版.exe",
        "SiglusEngineCHS.exe",
        "game.exe",
    ] {
        assert!(!is_plumbing(name), "{name} 是游戏，不能当成管道进程");
    }
}

#[test]
fn does_not_match_unrelated_processes() {
    assert!(!matches("game.exe", "wineserver", "/usr/bin/wineserver\0"));
    assert!(!matches(
        "game.exe",
        "game2.exe",
        "C:\\games\\other\\game2.exe\0"
    ));
    // An empty want never matches.
    assert!(!matches("", "game.exe", "game.exe\0"));
}

#[tokio::test]
async fn waiting_for_a_missing_process_returns_immediately() {
    let started = std::time::Instant::now();
    wait_until_gone("kotori-definitely-not-running").await;
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
}

/// 自动追踪靠它回答"这一款是不是已经有会话了":会话里记的名字来自配置或 exe
/// 文件名,两个写法都得能对上,而空名字不许跟任何东西相等(否则第一次轮询就会
/// 把每个游戏都当成"已经在跟")。
#[test]
fn two_spellings_of_the_same_process_name_match() {
    assert!(same_name("game.exe", "C:\\games\\demo\\game.exe"));
    assert!(same_name("Game.EXE", "game.exe"));
    // `/proc/<pid>/comm` 只留 15 字节,长名字在进程表里就是这个截断形式
    // (名字全是 ASCII,所以按字节切与内核一致)。
    let long = "VeryLongGameName.exe";
    assert!(same_name(long, &long[..15]));
    assert!(!same_name("game.exe", "game2.exe"));
    assert!(!same_name("", "game.exe"));
    assert!(!same_name("game.exe", ""));
}

#[test]
fn descendants_come_from_one_pass_and_keep_their_names() {
    // The shape both platform implementations share: one process table,
    // BFS from the root, names carried along for the plumbing check.
    let table = vec![
        (1, 0, "init".to_string()),
        (10, 1, "shell".to_string()),
        (11, 10, "game.exe".to_string()),
        (12, 10, "winedevice.exe".to_string()),
        (13, 11, "game.exe".to_string()),
        (14, 99, "unrelated".to_string()),
    ];
    let (pids, named) = collect_descendants(10, &table);
    assert_eq!(pids, vec![11, 12, 13]);
    assert_eq!(
        named,
        vec![
            (11, "game.exe".to_string()),
            (12, "winedevice.exe".to_string()),
            (13, "game.exe".to_string())
        ]
    );
}

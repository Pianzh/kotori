//! Process detection:回答「这个游戏还在跑吗、它的进程树里都有谁」。
//!
//! 按平台拆成两个实现(`unix.rs` 轮询 `/proc`,`windows.rs` 打一份 Toolhelp 快照),
//! 匹配逻辑与「游戏 vs wine 管道进程」的判定留在本文件 —— 它们平台无关,测试也
//! 因此两边都能跑。对外的函数签名与拆分前一致,调用点一行都不用改。
//!
//! Wine process names are awkward to match:
//!   * `/proc/<pid>/comm` is truncated to 15 bytes (`TASK_COMM_LEN`), so
//!     `SiglusEngineCHS.exe` shows up as `SiglusEngineCH`;
//!   * wine rewrites `argv[0]` to a Windows path (`C:\games\x\game.exe`),
//!     while native helpers keep a Unix path.
//!
//! So both are inspected, case-insensitively, after stripping any directory
//! part and trying the truncated form as well. (Windows has neither problem —
//! the snapshot reports the full exe name — and simply passes it as `comm`.)

use std::collections::HashMap;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{descendants, find_pids, live_game_processes};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{descendants, find_pids, live_game_processes};

/// `C:\games\x\Game.exe` / `/usr/bin/wine` -> `game.exe` / `wine`
pub fn normalize_process_name(name: &str) -> String {
    let name = name.trim();
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_matches('"');
    base.to_lowercase()
}

/// The part of `name` that `/proc/<pid>/comm` can still hold.
///
/// `TASK_COMM_LEN` is 16 *bytes* including the terminator, so the kernel keeps 15
/// bytes — not 15 characters. Taking 15 characters was wrong for every non-ASCII
/// name: a Chinese exe name is 3 bytes per character, so the kernel stores about
/// five of them and a 15-character guess matches nothing at all.
fn truncated(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if out.len() + ch.len_utf8() > 15 {
            break;
        }
        out.push(ch);
    }
    out
}

/// Does one process match the wanted name?
///
/// `seen` is the process name the platform reports — `/proc/<pid>/comm` on
/// Linux (truncated to 15 bytes) or the Toolhelp exe name on Windows
/// (complete). `cmdline` is the NUL-separated command line (empty on Windows):
/// wine rewrites `argv[0]` to a Windows path, so the first element is matched
/// too.
fn matches(needle: &str, seen: &str, cmdline: &str) -> bool {
    let needle = normalize_process_name(needle);
    if needle.is_empty() {
        return false;
    }

    let seen = seen.trim().to_lowercase();
    if !seen.is_empty() && (seen == needle || seen == truncated(&needle)) {
        return true;
    }

    let argv0 = cmdline.split('\0').next().unwrap_or_default();
    let argv0 = normalize_process_name(argv0);
    !argv0.is_empty() && argv0 == needle
}

/// Is a process with this name running?
pub fn is_running(name: &str) -> bool {
    !find_pids(name).is_empty()
}

/// 两个名字是不是"同一个进程"?按 [`matches`] 那套规矩比(去掉目录、大小写不敏感、
/// 也认 15 字节截断形式)。
///
/// 给"自动追踪"那条路用:它要在**会话表**里认出"这一款已经有会话了",而会话里记的
/// 进程名可能来自配置(`process_name`),也可能是 exe 文件名 —— 两者都得能对上。
pub fn same_name(left: &str, right: &str) -> bool {
    let left = normalize_process_name(left);
    let right = normalize_process_name(right);
    !left.is_empty()
        && !right.is_empty()
        && (left == right || left == truncated(&right) || right == truncated(&left))
}

/// Poll interval used while waiting for a watched game.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a watch-only session waits for the game to show up before giving
/// up (the user may click "monitor" and then start the game).
pub const APPEAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Wait until no process matches `name`. Returns immediately when it is not
/// running right now.
pub async fn wait_until_gone(name: &str) {
    if !is_running(name) {
        return;
    }
    tracing::info!("watched process {name} is running, waiting for it to exit");
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if !is_running(name) {
            tracing::info!("watched process {name} exited");
            return;
        }
    }
}

/// Processes that belong to wine's plumbing or to gamescope, not to a game.
///
/// Wine runs a set of helper processes per prefix, and the measured reason a
/// session can hang is that one of them (`winedevice.exe`) ignores SIGTERM and
/// outlives the game by forever. So "the process tree is not empty" must not be
/// read as "the game is still running"; this list is what separates the two.
const PLUMBING: [&str; 16] = [
    "gamescope",
    "gamescope-wl",
    "gamescopereaper",
    "xwayland",
    "wine",
    "wine64",
    "wineserver",
    "wine-preloader",
    "wine64-preloader",
    "services.exe",
    "winedevice.exe",
    "plugplay.exe",
    "rpcss.exe",
    "svchost.exe",
    "explorer.exe",
    "conhost.exe",
];

/// Is this the name of plumbing rather than of a game?
pub fn is_plumbing(name: &str) -> bool {
    PLUMBING.contains(&normalize_process_name(name).as_str())
}

/// Every live descendant of `root`, as (pid, name).
///
/// Both platforms answer from **one** pass over the process table instead of
/// following parent links upwards from each candidate: this runs exactly when a
/// session's tree is falling apart, and a process whose parent has already died
/// is still listed under that old parent here — which is the only way to still
/// find it.
pub(crate) fn collect_descendants(
    root: i32,
    table: &[(i32, i32, String)],
) -> (Vec<i32>, Vec<(i32, String)>) {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for (pid, parent, _) in table {
        children.entry(*parent).or_default().push(*pid);
    }

    let mut pids = Vec::new();
    let mut named = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            pids.push(child);
            if let Some((_, _, name)) = table.iter().find(|(p, _, _)| *p == child) {
                named.push((child, name.clone()));
            }
            queue.push(child);
        }
    }
    (pids, named)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

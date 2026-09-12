//! Process detection, used to tell when a watched game is running.
//!
//! Wine process names are awkward to match:
//!   * `/proc/<pid>/comm` is truncated to 15 bytes (`TASK_COMM_LEN`), so
//!     `SiglusEngineCHS.exe` shows up as `SiglusEngineCH`;
//!   * wine rewrites `argv[0]` to a Windows path (`C:\games\x\game.exe`),
//!     while native helpers keep a Unix path.
//!
//! So both are inspected, case-insensitively, after stripping any directory
//! part and trying the truncated form as well.

use std::collections::HashMap;

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

/// Does one process (already read from `/proc`) match the wanted name?
fn matches(needle: &str, comm: &str, cmdline: &str) -> bool {
    let needle = normalize_process_name(needle);
    if needle.is_empty() {
        return false;
    }

    let comm = comm.trim().to_lowercase();
    if !comm.is_empty() && (comm == needle || comm == truncated(&needle)) {
        return true;
    }

    let argv0 = cmdline.split('\0').next().unwrap_or_default();
    let argv0 = normalize_process_name(argv0);
    !argv0.is_empty() && argv0 == needle
}

/// PIDs of running processes whose name matches `name`.
pub fn find_pids(name: &str) -> Vec<i32> {
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };

    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };

        let dir = entry.path();
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
        let cmdline = std::fs::read_to_string(dir.join("cmdline")).unwrap_or_default();
        if matches(name, &comm, &cmdline) {
            pids.push(pid);
        }
    }

    pids.sort_unstable();
    pids
}

/// Is a process with this name running?
pub fn is_running(name: &str) -> bool {
    !find_pids(name).is_empty()
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

/// Every live descendant of `root`.
///
/// Built from one pass over `/proc` instead of following parent links upwards
/// from each candidate: this runs exactly when a session's tree is falling apart,
/// and a process whose parent has already died is still listed under that old
/// parent here — which is the only way to still find it.
pub fn descendants(root: i32) -> Vec<i32> {
    if root <= 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some(parent) = parent_of(pid) {
            children.entry(parent).or_default().push(pid);
        }
    }

    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            found.push(child);
            queue.push(child);
        }
    }
    found
}

/// `PPid` from `/proc/<pid>/status`.
///
/// The `status` file rather than `stat`: `stat`'s second field is the command
/// name in parentheses, and a command name may itself contain spaces and
/// parentheses, which is a classic way to misparse it.
fn parent_of(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|rest| rest.trim().parse::<i32>().ok())
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

/// Descendants of `root` that look like a game: pid and `/proc/<pid>/comm`.
///
/// The second opinion before kotori ends a session on the strength of a single
/// process name: a launcher hands off to the real game and exits first, and the
/// handoff target is what turns up here.
pub fn live_game_processes(root: i32) -> Vec<(i32, String)> {
    descendants(root)
        .into_iter()
        .filter_map(|pid| {
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            let name = name.trim().to_string();
            (!name.is_empty() && !is_plumbing(&name)).then_some((pid, name))
        })
        .collect()
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
    fn finds_descendants_and_calls_the_game_one_a_game() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        let own = std::process::id() as i32;

        assert!(
            descendants(own).contains(&pid),
            "子进程 {pid} 应该出现在自己的后代里"
        );

        // `spawn` returns on fork, before the child has exec'd, so its name can
        // briefly still be this test binary — poll instead of asserting at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !live_game_processes(own)
            .iter()
            .any(|(p, name)| *p == pid && name == "sleep")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "spawned sleep was never reported as a game process"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        child.kill().unwrap();
        child.wait().unwrap();
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

    #[test]
    fn finds_this_test_binary_and_a_spawned_process() {
        // Our own process is visible under its own name...
        let own = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = own.id() as i32;

        // `spawn` returns on fork, before the child has necessarily exec'd, so
        // its name can briefly still be this test binary — poll instead of
        // asserting immediately.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !find_pids("sleep").contains(&pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "spawned sleep (pid {pid}) never showed up as `sleep`"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(is_running("sleep"));

        // ...and disappears once it is gone.
        let mut own = own;
        own.kill().unwrap();
        own.wait().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while find_pids("sleep").contains(&pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !find_pids("sleep").contains(&pid),
            "killed process still reported as running"
        );
    }

    #[tokio::test]
    async fn waiting_for_a_missing_process_returns_immediately() {
        let started = std::time::Instant::now();
        wait_until_gone("kotori-definitely-not-running").await;
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}

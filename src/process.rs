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

/// The first 15 characters, which is all `/proc/<pid>/comm` can hold.
fn truncated(name: &str) -> String {
    name.chars().take(15).collect()
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

        let found = find_pids("sleep");
        assert!(
            found.contains(&pid),
            "expected to find the spawned sleep (pid {pid}) in {found:?}"
        );
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

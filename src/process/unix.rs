//! Linux 的进程表:`/proc` 逐个读 `comm` / `cmdline` / `status`。
//!
//! 与 `windows.rs` 的 Toolhelp 快照互为对方的平台实现,共用 `mod.rs` 的匹配逻辑;
//! 那 15 字节的 `comm` 截断只存在于这一侧(坑见 `mod.rs` 文件头)。

use super::{collect_descendants, is_plumbing, matches};

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

/// `(pid, ppid, comm)` for every process this user can see. `comm` arrives
/// with a trailing newline — trimmed here, because the name is what callers
/// compare and display.
fn process_table() -> Vec<(i32, i32, String)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let name = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            Some((pid, parent_of(pid)?, name.trim().to_string()))
        })
        .collect()
}

/// Every live descendant of `root`.
///
/// Built from one pass over `/proc` instead of following parent links upwards
/// from each candidate — the reasoning lives in [`super`]'s
/// `collect_descendants` (a process whose parent has already died is still
/// listed under that old parent here, which is the only way to still find it).
pub fn descendants(root: i32) -> Vec<i32> {
    if root <= 0 {
        return Vec::new();
    }
    collect_descendants(root, &process_table()).0
}

/// Descendants of `root` that look like a game: pid and `/proc/<pid>/comm`.
///
/// The second opinion before kotori ends a session on the strength of a single
/// process name: a launcher hands off to the real game and exits first, and the
/// handoff target is what turns up here.
pub fn live_game_processes(root: i32) -> Vec<(i32, String)> {
    if root <= 0 {
        return Vec::new();
    }
    collect_descendants(root, &process_table())
        .1
        .into_iter()
        .filter(|(_, name)| !name.is_empty() && !is_plumbing(name))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(super::super::is_running("sleep"));

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
}

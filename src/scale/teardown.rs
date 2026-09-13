//! Bounding a gamescope shutdown that would otherwise never finish.
//!
//! Everything here exists because of one measured fact: closing a game window
//! leaves gamescope in an unbounded `waitpid` loop, and the process is the only
//! place that is visible from outside. See [`stuck_in_teardown`] for the whole
//! story; the short version is that wine's `winedevice.exe` ignores SIGTERM and
//! gamescope waits for it forever, so somebody has to put a ceiling on it.

use std::time::Duration;

/// Check whether a process (by pid) still exists.
pub(super) fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM means the process exists but belongs to another user.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// What `/proc/<pid>/wchan` contains while a process is parked in `wait4()`.
///
/// The kernel reports the frame the task sleeps in, and that name has moved
/// around between versions, so every spelling seen in the wild is here.
const WAITING_FOR_CHILDREN: [&str; 3] = ["do_wait", "kernel_wait4", "wait4"];

/// Does a `/proc/<pid>/wchan` reading mean "this process is waiting for children"?
fn wchan_is_waiting_for_children(name: &str) -> bool {
    WAITING_FOR_CHILDREN.contains(&name)
}

/// Read `/proc/<pid>/wchan`, trimmed. `None` when it cannot be read — the process
/// is gone, it is a zombie (empty), or the kernel hides the symbol.
fn read_wchan(pid: i32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/wchan")).ok()?;
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Is this gamescope **stuck inside its own shutdown**?
///
/// Measured on the real machine (2026-09-12), because none of this is guessable.
/// Closing the window — the ✕, Alt+F4, anything the compositor turns into a close
/// request — makes gamescope run `KillAllChildren(SIGTERM)` and then
/// `WaitForAllChildren()`, which is an unbounded `waitpid(-1)` loop
/// (`src/Utils/Process.cpp`). Wine's `winedevice.exe` (the device-driver service)
/// does not act on SIGTERM, `gamescopereaper` waits for *it*, and gamescope waits
/// for the reaper: the main thread parks in `wait4()` **forever**. The window stays
/// mapped with nobody serving its event loop, so KWin decides it is not responding
/// and offers to kill it — the run that produced this note stayed like that for
/// two and a half minutes until KWin's helper did.
///
/// `wait4()` is only reachable once `wlserver_run()` has returned: the normal
/// state of that thread is the Wayland event loop (`poll_schedule_timeout` /
/// `do_epoll_wait`, both observed on the same run), and the other `waitpid` calls
/// in gamescope belong to the steamcompmgr thread and the reaper, not to the
/// thread this reads. So this being true means the session is on its way out and
/// only the cleanup is wedged — never a healthy game.
pub(super) fn stuck_in_teardown(pid: i32) -> bool {
    pid_alive(pid) && read_wchan(pid).is_some_and(|name| wchan_is_waiting_for_children(&name))
}

/// How long gamescope gets to finish shutting down on its own before kotori takes
/// the process group down.
///
/// The user's budget for a window on its way out is "one or two seconds"
/// (2026-09-12): past that, the wait is the bug rather than the exit.
pub(super) const TEARDOWN_GRACE: Duration = Duration::from_millis(1200);

/// How often the exit watchdog looks at gamescope's state.
pub(super) const TEARDOWN_POLL: Duration = Duration::from_millis(250);

/// How often the game's own process is looked for while a session runs.
///
/// Faster than [`TEARDOWN_POLL`] because this is what notices a game that quit
/// from its own menu, and the user is watching the window disappear when it does.
pub(super) const GAME_POLL: Duration = Duration::from_millis(500);

/// How long the game has to *stay* gone before the session counts as over.
///
/// One sighting of "not running" is not enough: wine starts its processes in
/// stages, and a game that is between two of them must not be mistaken for one
/// that has finished.
pub(super) const GAME_GONE_GRACE: Duration = Duration::from_millis(1500);

/// How long a session's processes get to act on SIGTERM before SIGKILL.
const GROUP_GRACE_STEPS: usize = 30;

/// How often the group is re-checked while it shuts down.
const GROUP_POLL: Duration = Duration::from_millis(100);

/// Bring a whole session down: its process group **and** the tree hanging off it.
///
/// Both, because they are not the same set. Measured (2026-09-12): wine's
/// `winedevice.exe` puts itself into a process group of its own, so killing the
/// session's group leaves it behind — and a left-behind `winedevice.exe` is
/// exactly what keeps `gamescopereaper` in `wait4()`, which keeps gamescope alive
/// and the session from ever ending (so the saves the game just wrote are never
/// uploaded).
pub(super) async fn terminate_session(root: i32) {
    if root <= 0 {
        return;
    }

    // Snapshot the tree *before* anything is signalled: once the root dies its
    // children are reparented, and a fresh walk would no longer find them.
    let mut tree = crate::process::descendants(root);
    signal(root, &tree, libc::SIGTERM);

    for _ in 0..GROUP_GRACE_STEPS {
        if !pid_alive(root) && tree.iter().all(|&pid| !pid_alive(pid)) {
            return;
        }
        tokio::time::sleep(GROUP_POLL).await;
    }

    tracing::warn!("会话进程组 {root} 没有自己收尾，kotori 直接 SIGKILL（连同它的整棵子进程树）");
    tree.extend(crate::process::descendants(root));
    signal(root, &tree, libc::SIGKILL);
}

/// Bring a session down **now**: no grace, straight to SIGKILL — but still the whole
/// tree, not just the group.
///
/// The only difference from [`terminate_session`] is the wait: there, SIGTERM is for
/// processes that can still act on it, while this is called once gamescope is already
/// parked in `wait4` and waiting longer changes nothing. The tree part is not
/// optional: `kill(-pgid)` alone leaves `winedevice.exe` behind, because it puts
/// itself in a process group of its own (measured 2026-09-12).
pub(super) fn kill_session_now(root: i32) {
    if root <= 0 {
        return;
    }
    let tree = crate::process::descendants(root);
    signal(root, &tree, libc::SIGKILL);
}

/// Signal the group *and* each pid in the tree.
fn signal(root: i32, tree: &[i32], signal: i32) {
    unsafe {
        libc::kill(-root, signal);
        for &pid in tree {
            libc::kill(pid, signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_wait4_park_counts_as_a_stuck_teardown() {
        // Exactly what the real machine reported while wedged, and the states the
        // same thread was in either side of it. A false positive here would kill a
        // game that is simply running.
        assert!(wchan_is_waiting_for_children("do_wait"));
        for healthy in [
            "poll_schedule_timeout.constprop.0", // idle in the Wayland event loop
            "do_epoll_wait",
            "0",                   // running
            "futex_wait_queue_me", // joining the steamcompmgr thread
        ] {
            assert!(
                !wchan_is_waiting_for_children(healthy),
                "{healthy} 不是收尾等待，不能当成卡死"
            );
        }
    }

    #[test]
    fn a_missing_process_has_no_wait_channel() {
        // `/proc/<pid>/wchan` of a pid that does not exist must not read as "stuck".
        assert_eq!(read_wchan(4_242_424), None);
        assert!(!stuck_in_teardown(4_242_424));
        assert!(!stuck_in_teardown(0));
    }

    #[tokio::test]
    async fn killing_nothing_is_also_harmless() {
        // `kill(-0, …)` 是"朝我自己的进程组开枪",所以 pid 0 必须被挡在门外 ——
        // 这条路径没有 SIGTERM 的缓冲,打错就是整个进程组立刻暴毙。
        kill_session_now(0);
        kill_session_now(4_242_424);
    }

    #[tokio::test]
    async fn terminating_nothing_is_instant_and_harmless() {
        // pid 0 must not be signalled at all (`kill(-0, …)` means "my own process
        // group"), and a session that is already gone must not sit out the grace
        // period — this runs on the daemon's hot path when a game exits.
        let started = std::time::Instant::now();
        terminate_session(0).await;
        terminate_session(4_242_424).await;
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}

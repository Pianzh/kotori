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
}

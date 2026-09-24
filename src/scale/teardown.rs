//! Bounding a gamescope shutdown that would otherwise never finish.
//!
//! Everything here exists because of one measured fact: closing a game window
//! leaves gamescope in an unbounded `waitpid` loop, and the process is the only
//! place that is visible from outside. See [`stuck_in_teardown`] for the whole
//! story; the short version is that wine's `winedevice.exe` ignores SIGTERM and
//! gamescope waits for it forever, so somebody has to put a ceiling on it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use super::ScaleSession;

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

/// 「这个 prefix 上还有没有**别的**会话在用」—— 没有才轮到我们关它。
///
/// `wineserver -k` 会把这个 prefix 上的进程一起带走（见 [`crate::wine::close_prefix`]），
/// 而"全局 `~/.wine`"这种共用 prefix 很常见：一款退出不该让另一款跟着掉线（BUG-22）。
/// `except` 是正在收尾的这一局（它自己可能还在会话表里，`None` = 表里已经没有它）。
pub(super) async fn prefix_is_unshared(
    sessions: &Arc<RwLock<HashMap<String, ScaleSession>>>,
    prefix: &Path,
    except: Option<&str>,
) -> bool {
    !sessions.read().await.values().any(|session| {
        Some(session.session_id.as_str()) != except
            && session.wine_prefix.as_deref() == Some(prefix)
    })
}

/// 收尾时关掉这一局的 wine server —— **只有这个 prefix 上没有别的会话**时才关。
///
/// 它原来住在 `gamescope.rs`，搬到这儿有两个理由：一是它讲的正是收尾（这个模块的
/// 题目），二是 `gamescope.rs` 已经七百多行、不该再往上加。搬过来之后那个判断还能
/// 被单测直接验（下面 `a_prefix_another_session_still_uses_is_left_alone`）。
pub(super) async fn close_wine_unshared(
    sessions: &Arc<RwLock<HashMap<String, ScaleSession>>>,
    prefix: Option<&Path>,
    except: Option<&str>,
) {
    let Some(prefix) = prefix else {
        return;
    };
    if !prefix_is_unshared(sessions, prefix, except).await {
        tracing::info!(
            "prefix {} 还有别的会话在用，不关它的 wine server",
            prefix.display()
        );
        return;
    }
    crate::wine::close_prefix(prefix).await;
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

    /// 造一个只关心 `session_id` 与 `wine_prefix` 的会话（其余字段占位）。
    fn session(id: &str, prefix: Option<&Path>) -> ScaleSession {
        ScaleSession {
            session_id: id.to_string(),
            game_id: Some(format!("game-{id}")),
            gamescope_pid: None,
            profile: crate::config::ScaleProfile::default_for(),
            output_size: (0, 0),
            runtime_ratio: 1.0,
            started_at: std::time::Instant::now(),
            process_group: None,
            process_name: None,
            exe_path: None,
            wine_prefix: prefix.map(Path::to_path_buf),
            watch_only: false,
            direct: false,
        }
    }

    /// BUG-22 的回归：**共用一个 prefix 的两个会话**里，一个收尾不该把另一个的
    /// wine server 一起关掉（`wineserver -k` 会把那个 prefix 上的一切带走）。
    #[tokio::test]
    async fn a_prefix_another_session_still_uses_is_left_alone() {
        let sessions: Arc<RwLock<HashMap<String, ScaleSession>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let shared = Path::new("/home/me/.wine");
        sessions
            .write()
            .await
            .insert("a".into(), session("a", Some(shared)));
        sessions
            .write()
            .await
            .insert("b".into(), session("b", Some(shared)));

        assert!(
            !prefix_is_unshared(&sessions, shared, Some("a")).await,
            "b 还在用这个 prefix，不该轮到 a 去关"
        );
        // 自己不算"别人"：表里只剩自己时，那就是该关的时候。
        assert!(!prefix_is_unshared(&sessions, shared, None).await);

        // b 走了 ⇒ 才轮到关。
        sessions.write().await.remove("b");
        assert!(prefix_is_unshared(&sessions, shared, Some("a")).await);

        // 用**别的** prefix 的会话不影响判断。
        sessions.write().await.insert(
            "c".into(),
            session("c", Some(Path::new("/games/other/prefix"))),
        );
        assert!(prefix_is_unshared(&sessions, shared, Some("a")).await);

        // 没有 prefix 的会话（观测会话）也不算数。
        sessions
            .write()
            .await
            .insert("d".into(), session("d", None));
        assert!(prefix_is_unshared(&sessions, shared, Some("a")).await);
    }
}

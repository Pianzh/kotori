//! Linux 的进程表:`/proc` 逐个读 `comm` / `cmdline` / `status`。
//!
//! 与 `windows.rs` 的 Toolhelp 快照互为对方的平台实现,共用 `mod.rs` 的匹配逻辑;
//! 那 15 字节的 `comm` 截断只存在于这一侧(坑见 `mod.rs` 文件头)。

use super::{Pickable, ProcEntry, collect_descendants, is_plumbing, matches};
use std::path::PathBuf;

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

/// 一份进程表快照:pid + comm + cmdline。见 [`super::Snapshot`]。
pub fn snapshot() -> Vec<ProcEntry> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let dir = entry.path();
            Some(ProcEntry {
                pid,
                name: std::fs::read_to_string(dir.join("comm")).unwrap_or_default(),
                cmdline: std::fs::read_to_string(dir.join("cmdline")).unwrap_or_default(),
            })
        })
        .collect()
}

/// 可以挑的进程:命令行或进程名以 `.exe` 结尾的那些(wine 跑的游戏)。
///
/// Linux 这边**拿不到窗口标题**,所以名字本身就是用户唯一的线索 —— 好在
/// `/proc/<pid>/cmdline` 里 wine 会把 exe 的完整路径写出来(常常是 `Z:\...\game.exe`
/// 这种 Windows 形状,见 [`unix_exe_path`]),那正是"添加游戏"要填的东西。
pub fn pickable() -> Vec<Pickable> {
    snapshot()
        .into_iter()
        .filter(|entry| !is_plumbing(&entry.name))
        .filter_map(|entry| {
            let name = entry.display_name();
            let argv0 = entry.cmdline.split('\0').next().unwrap_or_default().trim();
            let game_like =
                name.to_lowercase().ends_with(".exe") || argv0.to_lowercase().ends_with(".exe");
            // 相对路径要靠**进程自己的 cwd** 才能变成可用的路径,而 cwd 只在真要挑
            // 进程时才读(快照那条路每 2 秒跑一次,不能顺手多读一遍);`then` 的闭包
            // 因此是"真像游戏才去读"。
            game_like.then(|| Pickable {
                pid: entry.pid,
                name,
                title: String::new(),
                exe: exe_path(entry.pid, &entry.cmdline)
                    .map(|path| path.to_string_lossy().to_string()),
            })
        })
        .collect()
}

/// 命令行里那个 exe 换成本机路径。
///
/// * 本来就是绝对 Unix 路径 → 原样;
/// * `Z:\run\media\…\game.exe` → `/run/media/…/game.exe`(`Z:` 在 wine 里就是 `/`);
/// * 别的盘符(`C:\…` 在某个 prefix 里,而这里不知道是哪个 prefix) → `None`,
///   让人自己填 —— 猜错比不猜更难查。
pub(super) fn unix_exe_path(argv0: &str, cwd: Option<&str>) -> Option<String> {
    let text = argv0.trim();
    if text.starts_with('/') {
        return Some(text.to_string());
    }
    // 相对路径(有些启动器就这么写):拿进程自己的 cwd 拼出来。读不到 cwd 就不猜。
    if !text.is_empty()
        && !text.contains(':')
        && let Some(cwd) = cwd.map(str::trim).filter(|dir| dir.starts_with('/'))
    {
        return Some(format!("{}/{}", cwd.trim_end_matches('/'), text));
    }
    let (drive, rest) = text.split_once(':')?;
    if !drive.eq_ignore_ascii_case("z") {
        return None;
    }
    let rest = rest.trim_start_matches(['\\', '/']).replace('\\', "/");
    (!rest.is_empty()).then(|| format!("/{rest}"))
}

/// 这个进程的 exe 完整路径:命令行首项(可能是 `Z:\…` / 相对路径)换成本机路径。
///
/// Linux 这边 `/proc/<pid>/exe` 不能用:跑 wine 游戏时它指向的是 **wine 载入器**,
/// 不是游戏 —— 游戏的真实身份只能从 `argv[0]` 里读(wine 会把它改写成游戏自己的
/// 路径)。相对路径要靠**进程自己的 cwd** 才拼得出来,所以这里读一次
/// `/proc/<pid>/cwd`;`C:\…` 那种盘符在不知道是哪个 prefix 的情况下不猜(见
/// [`unix_exe_path`])—— 那时返回 `None`,自动追踪就不认这个进程。
pub fn exe_path(pid: i32, cmdline: &str) -> Option<PathBuf> {
    let argv0 = cmdline.split('\0').next().unwrap_or_default().trim();
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
    unix_exe_path(argv0, cwd.as_deref().and_then(|dir| dir.to_str())).map(PathBuf::from)
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

//! Windows 的进程表:一份 Toolhelp 快照回答"谁在跑、谁是誰的孩子"。
//!
//! 快照是**一致性切面**——`CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` 给出
//! 调用那一刻的全部进程(pid、父 pid、完整 exe 名),`descendants` 因此与 Linux
//! 侧同构:一次遍历建父子表,再从根 BFS。exe 名没有 Linux `comm` 的 15 字节截断,
//! 匹配直接命中完整名(截断分支无害,见 `mod.rs` 的 `matches`)。
//!
//! ⚠ 会话边界在这里比 Linux 侧**精确**:Windows 等进程句柄 `WaitForSingleObject`
//! 的路子(`PLATFORMS.md` §2.2)将来可以换掉轮询,但观测接口先保持与 Linux 一致,
//! 让 `watch_only` 与收尾判定先用起来。

use super::{collect_descendants, is_plumbing, matches};

/// One process as the snapshot reports it: pid, parent pid, full exe name.
struct Entry {
    pid: i32,
    parent: i32,
    name: String,
}

/// 一份进程表快照:`(exe 名, 命令行)`。Windows 这边拿不到命令行,给空串 ——
/// 匹配逻辑对空命令行本来就不做额外判断(见 `mod.rs` 的 `matches`)。
pub fn snapshot() -> Vec<(String, String)> {
    process_table()
        .into_iter()
        .map(|entry| (entry.name, String::new()))
        .collect()
}

/// One consistent pass over the process table.
fn process_table() -> Vec<Entry> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if handle == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(handle, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                out.push(Entry {
                    pid: entry.th32ProcessID as i32,
                    parent: entry.th32ParentProcessID as i32,
                    name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                });
                if Process32NextW(handle, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(handle);
    }
    out
}

/// PIDs of running processes whose exe name matches `name`.
pub fn find_pids(name: &str) -> Vec<i32> {
    let mut pids: Vec<i32> = process_table()
        .into_iter()
        .filter(|entry| matches(name, &entry.name, ""))
        .map(|entry| entry.pid)
        .collect();
    pids.sort_unstable();
    pids
}

/// Every live descendant of `root` (see `mod.rs` for why one pass wins).
pub fn descendants(root: i32) -> Vec<i32> {
    if root <= 0 {
        return Vec::new();
    }
    let table: Vec<(i32, i32, String)> = process_table()
        .into_iter()
        .map(|entry| (entry.pid, entry.parent, entry.name))
        .collect();
    collect_descendants(root, &table).0
}

/// Descendants of `root` that look like a game: pid and exe name, plumbing
/// filtered out (same list as the Linux side feeds into the teardown decision).
pub fn live_game_processes(root: i32) -> Vec<(i32, String)> {
    if root <= 0 {
        return Vec::new();
    }
    let table: Vec<(i32, i32, String)> = process_table()
        .into_iter()
        .map(|entry| (entry.pid, entry.parent, entry.name))
        .collect();
    collect_descendants(root, &table)
        .1
        .into_iter()
        .filter(|(_, name)| !name.is_empty() && !is_plumbing(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_sees_this_test_process() {
        let exe = std::env::current_exe().unwrap();
        let own_name = exe.file_name().unwrap().to_string_lossy().to_string();
        let own = std::process::id() as i32;

        assert!(
            find_pids(&own_name).contains(&own),
            "当前测试进程({own_name}, pid {own})应该出现在快照里"
        );
    }

    #[test]
    fn a_spawned_child_shows_up_in_the_tree() {
        // `sleep` does not exist on Windows; `ping -n` is the classic stand-in
        // (one probe per second, so 20 keeps the child alive for the test).
        let mut child = std::process::Command::new("ping")
            .args(["-n", "20", "127.0.0.1"])
            .spawn()
            .expect("spawn ping");
        let pid = child.id() as i32;
        let own = std::process::id() as i32;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !descendants(own).contains(&pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "spawned ping (pid {pid}) never showed up as a descendant"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            live_game_processes(own)
                .iter()
                .any(|(p, name)| *p == pid && name.starts_with("PING")),
            "ping 是游戏进程,不该被当成管道"
        );

        child.kill().unwrap();
        child.wait().unwrap();
    }
}

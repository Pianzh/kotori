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

use super::{Pickable, ProcEntry, collect_descendants, is_plumbing, matches};

/// One process as the snapshot reports it: pid, parent pid, full exe name.
struct Entry {
    pid: i32,
    parent: i32,
    name: String,
}

/// 一份进程表快照。Windows 这边拿不到命令行,`argv0` 给空串 ——
/// 匹配逻辑对空命令行本来就不做额外判断(见 `mod.rs` 的 `matches`)。
pub fn snapshot() -> Vec<ProcEntry> {
    process_table()
        .into_iter()
        .map(|entry| ProcEntry {
            pid: entry.pid,
            name: entry.name,
            cmdline: String::new(),
        })
        .collect()
}

/// 可以挑的进程:**有可见顶层窗口**的那些。
///
/// Windows 上"进程表"里有上百项(svchost、RuntimeBroker……),把它们倒给用户等于
/// 什么也没说。用户认得出的是**窗口**:一个可见的、没有属主的顶层窗口就是一个正在
/// 玩的游戏(属主非空的是对话框/工具窗,子窗口根本不在 `EnumWindows` 的结果里)。
///
/// 顺带把窗口标题带上 —— 那是"哪一款游戏"最直接的答案,而 exe 路径用来直接添加游戏。
pub fn pickable() -> Vec<Pickable> {
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible,
    };

    /// `EnumWindows` 的回调:收"可见、无属主、有标题"的顶层窗口,lparam 是那个 `Vec`。
    unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: 调用方保证 param 指向一个活着的 Vec<(u32, String)>,而 EnumWindows
        // 是同步的 —— 回调期间它一直在。
        let found = unsafe { &mut *(param as *mut Vec<(u32, String)>) };
        let visible = unsafe { IsWindowVisible(hwnd) } != 0;
        let owned = !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null();
        if visible && !owned {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            let len = unsafe { GetWindowTextLengthW(hwnd) };
            if pid != 0 && len > 0 {
                let mut buffer = vec![0u16; len as usize + 1];
                let written =
                    unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
                if written > 0 {
                    let title = String::from_utf16_lossy(&buffer[..written as usize]);
                    found.push((pid, title.trim().to_string()));
                }
            }
        }
        1
    }

    let mut windows: Vec<(u32, String)> = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM) };

    // 一个进程可能开着好几个窗口(主窗口 + 无属主的浮窗):留标题最长的那一个。
    let own = std::process::id();
    let names: std::collections::HashMap<i32, String> = process_table()
        .into_iter()
        .map(|entry| (entry.pid, entry.name))
        .collect();
    let mut seen: Vec<(u32, String)> = Vec::new();
    for (pid, title) in windows {
        if pid == own {
            continue;
        }
        match seen.iter_mut().find(|(seen_pid, _)| *seen_pid == pid) {
            Some(entry) if title.chars().count() > entry.1.chars().count() => entry.1 = title,
            Some(_) => {}
            None => seen.push((pid, title)),
        }
    }

    seen.into_iter()
        .map(|(pid, title)| {
            let pid = pid as i32;
            // 拿 exe 路径:权限不够(系统进程)是常态,那就是 `None`,不是错误。
            // SAFETY: 句柄拿到就必须关;缓冲区是活的 Vec,`size` 由 API 回写。
            let exe = unsafe {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
                if handle.is_null() {
                    None
                } else {
                    let mut buffer = vec![0u16; 4096];
                    let mut size = buffer.len() as u32;
                    let ok =
                        QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) != 0;
                    CloseHandle(handle);
                    (ok && size > 0).then(|| String::from_utf16_lossy(&buffer[..size as usize]))
                }
            };
            Pickable {
                pid,
                name: names.get(&pid).cloned().unwrap_or_else(|| title.clone()),
                title,
                exe,
            }
        })
        .collect()
}

/// 这个 pid 还活着吗?Toolhelp 快照只列活着的进程,所以查表即可 ——
/// 比 `OpenProcess` 少一个句柄要关,而这里每 2 秒问一次。
pub fn pid_is_alive(pid: i32) -> bool {
    process_table().iter().any(|entry| entry.pid == pid)
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

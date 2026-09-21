//! Process detection:回答「这个游戏还在跑吗、它的进程树里都有谁」。
//!
//! **认人靠 exe 的完整路径**(`ProcEntry::exe_path` + `crate::util::same_file`),名字
//! 只当便宜的预筛 —— 库里两款游戏都叫 `Game.exe` 时,名字分不清谁是谁,而存档同步
//! 认错了游戏是要出事的(用户 2026-09-20)。
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
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{descendants, exe_path, find_pids, live_game_processes, pickable, snapshot};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::kill_tree;
#[cfg(windows)]
pub use windows::{exe_path, find_pids, pickable, snapshot};
// ⚠ Windows 侧的 `descendants` 只有**测试**要(`kill_tree` 在自己模块里直接调它,
//    而收尾那条游戏进程树的路是 unix 的 `teardown`),所以它只在 test 构建里再导出。
#[cfg(all(windows, test))]
pub use windows::descendants;

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

/// 进程表里的一条。
///
/// `name` 是平台报出来的进程名(`/proc/<pid>/comm`,只留 15 字节;Windows 是 Toolhelp
/// 的完整 exe 名),`cmdline` 是**NUL 分隔的整条命令行**(**Linux 有,Windows 拿不到,
/// 给空串**)——匹配规则两样都看,因为 wine 会把 `argv[0]` 改写成 Windows 路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: i32,
    pub name: String,
    pub cmdline: String,
}

impl ProcEntry {
    /// 这条跟 `name` 是不是同一个进程(规则与 [`is_running`] 一致)。
    pub fn matches(&self, name: &str) -> bool {
        matches(name, &self.name, &self.cmdline)
    }

    /// 这个进程的 exe **完整路径** —— 与任务管理器「详细信息」里那一栏同一个来源。
    ///
    /// 拿不到就是 `None`(`unix.rs` / `windows.rs` 各自写清了什么时候拿不到):它
    /// 只说明"认不出这是不是档案里那个 exe",不是错误。判断"是不是同一个文件"交给
    /// [`crate::util::same_file`] —— 同一个文件可以有多种写法。
    pub fn exe_path(&self) -> Option<PathBuf> {
        exe_path(self.pid, &self.cmdline)
    }

    /// **显示给用户**的名字。
    ///
    /// 优先命令行首项的文件名:它是完整的。平台报的那个 `comm` 在 Linux 上只留
    /// 15 字节(内核的 `TASK_COMM_LEN`),`kotori-observe-proc` 会变成
    /// `kotori-observe-` —— 拿它当"跟的是谁"报给用户,人家会以为跟错了东西
    /// (2026-09-20 实测就是这么显示出来的)。
    pub fn display_name(&self) -> String {
        // 命令行是 NUL 分隔的,只有**第一个字段**是 argv[0](后面是参数)。
        let argv0 = self.cmdline.split('\0').next().unwrap_or_default().trim();
        let base = argv0
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .trim_matches('"')
            .trim();
        if !base.is_empty() {
            return base.to_string();
        }
        self.name.trim().to_string()
    }
}

/// 某一刻的进程表快照:一次取,然后回答很多个名字(或很多个 pid)。
///
/// 逐个名字调 [`is_running`] 是**每个名字读一遍进程表** —— 自动追踪要盯配置里
/// 每一款开着追踪的游戏,42 款就是每 2 秒读 42 遍 `/proc`(Windows 那边是 42 次
/// Toolhelp 快照)。这里只取一次,匹配在内存里做。
pub struct Snapshot {
    entries: Vec<ProcEntry>,
}

impl Snapshot {
    /// 取一份当下的快照。
    pub fn take() -> Self {
        Self {
            entries: snapshot(),
        }
    }

    /// 这个 pid 现在还在进程表里吗?
    ///
    /// 只给测试用(`#[cfg(test)]`):产品代码问的是"这一款游戏还在不在",那是
    /// [`Snapshot::matches_exe`] 的事;测试问的是"这个进程死了没有",就是这个。
    #[cfg(test)]
    pub fn has_pid(&self, pid: i32) -> bool {
        self.entries.iter().any(|entry| entry.pid == pid)
    }

    /// 有没有哪个进程**就是** `exe` 这一个可执行文件?
    ///
    /// `name` 只是**便宜的预筛**:先按名字滤掉绝大多数进程,免得给每个进程都去读
    /// 一次 `/proc/<pid>/cwd`(或开一个句柄)。真正算数的是完整路径 —— 库里两款游戏
    /// 都叫 `Game.exe` 时,名字分不清,路径分得清(用户 2026-09-20:自动追踪的凭据
    /// 是 exe 目录 + exe 文件名,而这两样合起来就是完整路径)。
    pub fn matches_exe(&self, name: &str, exe: &Path) -> bool {
        self.entries
            .iter()
            .filter(|entry| entry.matches(name))
            .any(|entry| {
                entry
                    .exe_path()
                    .is_some_and(|path| crate::util::same_file(&path, exe))
            })
    }
}

/// 「从正在运行的进程里挑」的一个候选。
///
/// 给添加游戏页的「从运行中的进程添加」用:列表**刻意短**,不是把上百个进程倒给
/// 用户,而是只留那些"看着像游戏"的(Windows 侧 = 有可见顶层窗口的进程,Linux 侧
/// = `.exe`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickable {
    pub pid: i32,
    /// 显示用的进程名(完整的那一份,见 [`ProcEntry::display_name`])。
    pub name: String,
    /// 窗口标题 —— 用户真正认得出的东西(Windows 有;Linux 这边通常拿不到)。
    pub title: String,
    /// 可执行文件的完整路径;做不出来就是 `None`(用户自己在界面上补)。
    pub exe: Option<String>,
}

/// 此刻可以挑的进程。见 [`Pickable`]。
pub fn pickable_processes() -> Vec<Pickable> {
    let mut found = pickable();
    // 认得出的排前面(有标题的更认得出),其余按名字 —— 两个入口都吃这一份顺序。
    found.sort_by(|left, right| {
        right
            .title
            .is_empty()
            .cmp(&left.title.is_empty())
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.pid.cmp(&right.pid))
    });
    found
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
mod tests;

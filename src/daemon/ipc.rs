//! 守护进程和 UI/CLI 之间的那条本机连接。
//!
//! Linux 上是 Unix socket(`$XDG_RUNTIME_DIR/kotori.sock`),Windows 上是命名管道
//! (`\\.\pipe\kotori-<用户>`)。两者都是"本机、面向连接、由内核管权限"的东西,
//! 所以**上层只认这一层**:`Daemon::run` 和 `handle_client` 里没有一处 cfg。
//!
//! ⚠ 曾经的方向是让 Windows 上的 UI 和守护进程共用一个进程(进程内 `duplex`),
//! 好把"传输"这件事整个消掉。放弃了 —— 那等于推翻 GOALS §3.1 的双进程模型:
//! UI 一崩就把守护进程和正在跑的游戏一起带走,直接违反 §6.2。
//!
//! `socket_path` 这个名字一路上都留着:在 Windows 上它装的是管道名,`Path` 只是
//! 个方便的字符串容器(见 [`crate::config::default_socket_path`])。

use std::path::{Path, PathBuf};
use std::time::Duration;

/// 一条已经建立、可以直接读写的连接。
#[cfg(unix)]
pub(super) type Stream = tokio::net::UnixStream;
#[cfg(windows)]
pub(super) type Stream = tokio::net::windows::named_pipe::NamedPipeServer;

/// 监听端。
#[cfg(unix)]
pub(super) struct Listener {
    inner: tokio::net::UnixListener,
}

#[cfg(unix)]
impl Listener {
    pub(super) async fn bind(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 锁已经在手,所以这会儿还存在的 socket 文件只可能是**残留**:盖掉它。
        let _ = std::fs::remove_file(path);
        let inner = tokio::net::UnixListener::bind(path)
            .map_err(|e| anyhow::anyhow!("Failed to bind {}: {}", path.display(), e))?;
        Ok(Self { inner })
    }

    /// `&mut self` 只是为了和 Windows 那边的签名一致(那边要换出手里的待命实例),
    /// 这样调用点不用按平台分叉。
    pub(super) async fn accept(&mut self) -> std::io::Result<Stream> {
        self.inner.accept().await.map(|(stream, _addr)| stream)
    }
}

#[cfg(windows)]
pub(super) struct Listener {
    name: String,
    /// 已经建好、正等着被连上的那**一个**实例。
    ///
    /// 命名管道和 Unix socket 的形状差别就在这儿:监听端必须**预建**实例。如果等到
    /// `accept` 的时候才 `create`,那么"上一个实例刚被连上"到"下一个实例被创建"
    /// 之间就有一段**没有任何空闲实例**的窗口,客户端此刻 `open()` 拿到的是
    /// `ERROR_PIPE_BUSY`(231)—— 这在 Windows VM 上实测撞到了:UI 首次启动时
    /// 云同步页报连不上守护进程,点一下重试就好了,正是这个窗口。
    pending: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
impl Listener {
    pub(super) async fn bind(path: &Path) -> anyhow::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;

        // 管道名没有父目录要建,也没有残留文件要清:管道由内核持有,最后一个
        // 实例关掉它就没了。一开始就把第一个待命实例建出来。
        let name = path.as_os_str().to_string_lossy().into_owned();
        let pending = ServerOptions::new().create(&*name)?;
        Ok(Self { name, pending })
    }

    /// 命名管道的"接受"和 Unix 不是一个形状:每个连接都要**新开一个实例**。
    ///
    /// 关键在于顺序 —— **先把下一个实例建出来,再拿当前这个去等客户端**。反过来写
    /// 就会重新出现那个"没有空闲实例"的窗口(见 `pending` 的注释)。
    pub(super) async fn accept(&mut self) -> std::io::Result<Stream> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let next = ServerOptions::new().create(&*self.name)?;
        let current = std::mem::replace(&mut self.pending, next);
        // 这一步被取消(daemon 正好在收 shutdown 信号)只会丢掉 `current`,
        // `pending` 已经是新的了,所以取消之后照样有实例可连。
        current.connect().await?;
        Ok(current)
    }
}

/// 锁文件放哪。
///
/// Unix 上就贴着 socket(`kotori.sock` → `kotori.lock`)。Windows 上不行:那边的
/// `socket_path` 是**管道名**,不是文件系统路径,所以另找一处(和配置、日志
/// 一起放在数据目录里)。
#[cfg(unix)]
pub(super) fn lock_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("lock")
}

#[cfg(windows)]
pub(super) fn lock_path(_socket_path: &Path) -> PathBuf {
    crate::config::data_dir().join("daemon.lock")
}

/// 拿住"一个端点一个守护进程"的锁,拿不到就说清楚是谁占着。
///
/// 为什么用锁而不是"看看 socket 文件在不在":残留的**文件**正是 `Listener::bind`
/// 要清掉的东西,而一个**活着**的守护进程占着锁时不能被顶掉 —— 两个守护进程会同时
/// 写同一份配置,而"守护进程是唯一的写者"是这个项目的底线(ADR-002)。
///
/// `flock` 由内核持有,进程没了(SIGKILL 也算)锁就没了,所以崩溃的守护进程不会
/// 挡住继任者,锁文件本身可以一直留着(它是空的,而且在 runtime 目录里)。
#[cfg(unix)]
pub(super) fn claim_socket(lock_path: &Path) -> anyhow::Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| anyhow::anyhow!("无法创建锁文件 {}: {e}", lock_path.display()))?;
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !taken {
        anyhow::bail!(
            "已经有一个守护进程在跑（它占着 {}）。两个守护进程会同时写同一份配置，\
             所以这里不去抢它的 socket；要换掉它先跑 `kotori shutdown`。",
            lock_path.display()
        );
    }
    Ok(file)
}

/// Windows 版:没有 `flock`,用"独占打开"换到同样的保证。
///
/// 句柄随进程退出被系统关掉 —— 崩溃的守护进程不会挡住继任者,和 flock 的内核
/// 语义一致 —— 所以锁文件可以一直留着。
#[cfg(windows)]
pub(super) fn claim_socket(lock_path: &Path) -> anyhow::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        // 这个文件只是**锁的载体**(`share_mode(0)` 才是锁),里面从来不写内容,
        // 所以不截断:截断对"谁持有锁"没有任何影响,而说清楚比让 clippy 猜好。
        .truncate(false)
        .write(true)
        // 独占:别人再打开这个文件会失败,这就是 Windows 上的 flock。
        .share_mode(0)
        .open(lock_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "已经有一个守护进程在跑（它占着 {}；{e}）。两个守护进程会同时写\
                 同一份配置，所以这里不去抢它的管道；要换掉它先跑 `kotori shutdown`。",
                lock_path.display()
            )
        })
}

/// 打开守护进程的日志文件(追加),返回它的路径和句柄。
///
/// 两个平台的 `ensure_running` 只差"怎么连、怎么起",这块是一样的。
fn open_daemon_log() -> anyhow::Result<(PathBuf, std::fs::File)> {
    let log_dir = crate::config::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(super::DAEMON_LOG);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    Ok((log_path, log))
}

/// 守护进程没跑就把它拉起来,然后等到它的端点真的能连上。
///
/// GUI 和 CLI 都要这个:每一次启动游戏都得经过守护进程,所以谁先跑谁负责把它
/// 叫起来。
#[cfg(unix)]
pub fn ensure_running(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    if UnixStream::connect(socket).is_ok() {
        return Ok(());
    }
    tracing::info!("daemon 未运行，正在启动...");

    let (log_path, log) = open_daemon_log()?;

    std::process::Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log))
        // 自己的进程组:守护进程必须活得比"发给 UI/CLI 那组的信号"更久。
        .process_group(0)
        .spawn()
        .map_err(|e| anyhow::anyhow!("无法启动守护进程: {e}"))?;

    // 约 5 秒预算;这段跑在事件循环起来之前。
    for _ in 0..50 {
        if UnixStream::connect(socket).is_ok() {
            tracing::info!("daemon 已就绪");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!(
        "守护进程未在 5 秒内就绪（socket: {}，日志: {}）",
        socket.display(),
        log_path.display()
    )
}

/// 管道上现在有没有活着的守护进程在听。
///
/// ⚠ **这一处必须走 `std`,不能用 `tokio::net::windows::named_pipe`。** 后者要一个
/// 正在跑的 reactor,而 `ensure_running` 是**同步**函数,调用点不一定在 runtime 上下文
/// 里 —— CLI 的 `sync` / `scale` 就不是,于是 `ClientOptions::new().open()` 每次都在
/// `named_pipe.rs` 里 panic("there is no reactor running, must be called from the
/// context of a Tokio 1.x runtime")。用户 2026-09-18 在真机上撞到的是整个功能面:
/// 所有 `sync` / `scale` 子命令退出码 101(见 BUG-REPORT BUG-1)。
///
/// 这里只需要"通不通"这一个比特,`std::fs` 那层 `CreateFileW` 就够,而且它对管道名的
/// 处理和 `ClientOptions` 是同一条路:没有空闲实例时同样拿到 `ERROR_PIPE_BUSY`(231)。
/// 探针**会真的连上再立刻关掉**,这一点和改动前一致 —— 守护进程那边看到的是"连上又
/// 断开",`handle_client` 读一次 EOF 就收工;而 [`Listener::accept`] 是**先建下一个
/// 实例**再等的,所以这一次多余连接不会让紧随其后的真正调用撞上"没有空闲实例"。
///
/// 回归闸门见文件末尾的 `pipe_probe_does_not_need_a_runtime`。
#[cfg(windows)]
fn pipe_is_up(name: &str) -> bool {
    // 读写都要:守护进程那边的实例是 `ServerOptions` 默认的双工管道,真正的客户端
    // (`rpc.rs`)也是按双工打开的 —— 探针照抄这个权限,才代表得了"真连得上"。
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .is_ok()
}

/// Windows 版:同样的"连不上就起一个,再等它",只是端点是管道。
#[cfg(windows)]
pub fn ensure_running(socket: &Path) -> anyhow::Result<()> {
    use crate::util::exec::Quiet;

    let name = socket.as_os_str().to_string_lossy().into_owned();
    if pipe_is_up(&name) {
        return Ok(());
    }
    tracing::info!("daemon 未运行，正在启动...");

    let (log_path, log) = open_daemon_log()?;

    std::process::Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log))
        // 没有 `process_group(0)` 的对等物,也不需要:Windows 这边唯一会来的
        // "会话要结束了"是控制台 Ctrl-C,不会像 Unix 登出那样横扫一整个进程组。
        //
        // ⚠ 这个窗口不弹出来还有第二个理由:控制台一旦存在就会陪着守护进程活到它
        // 退出,而**用户点掉那个窗口等于给它发 Ctrl-C**(见 `util::exec`)。
        .quiet()
        .spawn()
        .map_err(|e| anyhow::anyhow!("无法启动守护进程: {e}"))?;

    for _ in 0..50 {
        if pipe_is_up(&name) {
            tracing::info!("daemon 已就绪");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!(
        "守护进程未在 5 秒内就绪（管道: {}，日志: {}）",
        name,
        log_path.display()
    )
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::pipe_is_up;

    /// BUG-1 的回归闸门:探针**在没有 runtime 的线程上**也必须能跑。
    ///
    /// `#[test]` 的线程不在任何 runtime 里,正是 CLI 的 `sync` / `scale` 所处的处境。
    /// 改动前这里是 `ClientOptions::new().open(...)`,在这个线程上直接 panic
    /// ("there is no reactor running"),所以这条测试红;换成 `std::fs` 之后它安静地
    /// 返回"通不了"。
    ///
    /// ⚠ 它只在 Windows 上编译,而 CI 的 `test-windows` 目前只做 `cargo check`
    /// (不跑测试),所以这条闸门**只在 Windows 上手动 `cargo test` 时真的跑**。
    /// 别因为它"在 CI 里是绿的"就以为它验过了。
    #[test]
    fn pipe_probe_does_not_need_a_runtime() {
        assert!(!pipe_is_up(r"\\.\pipe\kotori-nonexistent-probe"));
    }
}

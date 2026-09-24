// 在 Windows 上把 exe 声明成 **windows 子系统**:双击时不会先弹一个控制台黑框
// (GUI 就该是双击直接出窗口)。代价是 CLI 用法没有现成的 stdout/stderr,所以
// 启动时要 `attach_parent_console()` 附到终端上 —— 见那个函数的注释。
//
// 这个属性**不能**只给 release:开发时用的 debug 构建同样会双击,同样会看到黑框。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod cli;
mod config;
mod daemon;
mod desktop;
mod display;
mod game;
mod picker;
mod platform;
mod process;
mod rpc;
mod scale;
mod secrets;
mod sync;
mod ui;
mod util;
mod wine;
mod wine_prefixes;

use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

/// The GUI's own log, next to the daemon's.
const UI_LOG: &str = "ui.log";

/// Writes to stderr and, for the GUI, to a file as well.
///
/// The GUI is started from a terminal that the user closes — taking stderr
/// with it. When it then crashes "for no reason", the only evidence is gone.
/// Keeping a copy on disk makes the next one explainable.
#[derive(Clone, Default)]
struct LogWriter {
    file: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.write_all(buf);
        }
        std::io::stderr().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.flush();
        }
        std::io::stderr().flush()
    }
}

impl<'a> fmt::MakeWriter<'a> for LogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Open the GUI log, or `None` when it cannot be created (logging to stderr is
/// still better than not starting at all).
fn open_ui_log() -> Option<std::fs::File> {
    let dir = config::log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(UI_LOG))
        .ok()
}

/// 附到父进程的控制台上,让 CLI 输出还能到达终端。
///
/// exe 是 windows 子系统(见文件顶部的 `windows_subsystem`),系统**不会**给它分配
/// 控制台 —— 这正是双击不弹黑框的原因。但从终端里跑 `kotori status` 时,父进程
/// (PowerShell / cmd)是有控制台的,附上去 stdout/stderr 就又能用了。
///
/// 双击启动时没有父控制台,这个调用会失败 —— 无所谓:GUI 的日志走 `ui.log`。
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, SetConsoleCP, SetConsoleOutputCP,
    };

    /// 控制台 UTF-8。
    const CP_UTF8: u32 = 65001;

    // SAFETY: 三个都是一元的 Win32 调用,参数是它们规定的常量。失败也无所谓 ——
    // 双击启动时没有父控制台,这三个都会失败,而那种情况下本来就不需要控制台。
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            // 控制台默认代码页是 437(英文系统)/ 936(中文系统),而 kotori 打的是
            // UTF-8 字节 —— 实测在 Windows VM 的黑框里就是"一堆方框"。切成 UTF-8。
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }
}

fn main() -> anyhow::Result<()> {
    // 得**在日志初始化之前**:tracing 写 stderr,而 stderr 要先有地方可去。
    #[cfg(windows)]
    attach_parent_console();

    let cli = cli::Cli::parse();

    // 不给子命令就是启动 UI。双击 exe(Windows) / 点桌面图标(Linux)走的都是这条路,
    // 否则用户拿到的是一段 help,还得自己猜该敲哪个子命令。`--help` 照旧。
    let command = cli.command.unwrap_or(cli::Command::Ui);

    // Initialize logging.
    //
    // The graphics stack is chatty at `info`: on niri the wgpu Vulkan path
    // prints a `SurfaceError::Outdated` storm (tens of thousands of lines in
    // seconds), and drowning the terminal is itself a way to make the UI feel
    // frozen. Our own logs stay at `info`; the noisy modules are pushed to
    // `warn`, and `RUST_LOG` still overrides everything when debugging.
    const DEFAULT_LOG: &str = "info,\
         wgpu_core=warn,wgpu_hal=warn,wgpu_types=warn,naga=warn,\
         winit=warn,calloop=warn,sctk=warn,sctk_adwaita=warn";
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG)),
        )
        .with_writer(LogWriter {
            file: matches!(&command, cli::Command::Ui)
                .then(open_ui_log)
                .flatten()
                .map(|file| std::sync::Arc::new(std::sync::Mutex::new(file))),
        })
        .init();

    let rt = tokio::runtime::Runtime::new()?;

    match command {
        cli::Command::Reload => {
            // 配置是 daemon 在内存里持有的,而它也是唯一的写者。手改了
            // `config.toml` 之后不重读,下一次写就会把手改的内容盖掉。
            let socket = config::socket_path();
            match rt.block_on(async { rpc::call(&socket, "config.reload", None).await }) {
                Ok(value) => println!("已重读配置:{} 款游戏", value["games"].as_u64().unwrap_or(0)),
                Err(e) => {
                    println!("守护进程未响应（{}）: {e}", socket.display());
                    std::process::exit(1);
                }
            }
        }
        cli::Command::Daemon => {
            tracing::info!("Starting daemon mode");
            rt.block_on(async { daemon::run().await })?;
        }
        cli::Command::Ui => {
            tracing::info!("Starting UI mode");
            // Slint runs the event loop itself (it must be the main thread), but
            // the effects of the message loop are tokio futures — so this thread
            // has to be inside the runtime context for `Handle::current()` to
            // find it. The guard only needs to live as long as `ui::run`.
            let _guard = rt.enter();
            ui::run()?;
        }
        cli::Command::Launch { game_id } => {
            tracing::info!("Launching game: {}", game_id);
            // CLI front: wait for the game to exit (foreground).
            let session_id = rt.block_on(async { game::launch(&game_id, true).await })?;
            tracing::info!("session finished: {}", session_id);
        }
        cli::Command::List => {
            game::list()?;
        }
        cli::Command::Status => {
            let socket = config::socket_path();
            let status = rt.block_on(async { rpc::call(&socket, "daemon.status", None).await });
            match status {
                Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
                Err(e) => {
                    println!("守护进程未响应（{}）: {e}", socket.display());
                    std::process::exit(1);
                }
            }
        }
        cli::Command::Shutdown => {
            let socket = config::socket_path();
            match rt.block_on(async { rpc::call(&socket, "daemon.shutdown", None).await }) {
                Ok(_) => println!("守护进程已停止（正在运行的游戏不受影响）"),
                Err(e) => {
                    println!("守护进程未响应（{}）: {e}", socket.display());
                    std::process::exit(1);
                }
            }
        }
        cli::Command::Scan { directory } => {
            let games = game::scan(&directory)?;
            if games.is_empty() {
                println!("No games found in {}", directory.display());
            } else {
                println!("Found {} game(s) in {}:", games.len(), directory.display());
                // 展示也按 `add` 那套分配规则预演一遍：同一个 id 撞车时 `add` 会加
                // 后缀，展示若还直接 `generate_game_id`，三条不同的游戏会印成同一个
                // id（BUG-5，用户报过）。预演只动配置的副本，一个字节都不落盘。
                let mut preview = config::load()?;
                for g in &games {
                    let id = game::generate_unique_game_id(&preview, &g.name);
                    preview.games.insert(id.clone(), g.clone());
                    println!("  [{}] {} -> {}", id, g.name, g.exe_path.display());
                }
            }
        }
        cli::Command::Add { directory } => {
            let added = game::add_from_dir(&directory)?;
            if added.is_empty() {
                println!("No new games added from {}", directory.display());
            } else {
                println!("Added {} game(s):", added.len());
                for (id, g) in &added {
                    println!("  [{}] {} -> {}", id, g.name, g.exe_path.display());
                }
                // 重扫描的已有条目走的是"跳过"分支(不覆盖调好的档案),不会出现在
                // added 里;这里的警告只针对真正新加的、exe 又撞上别的档案的那几条。
                let config = config::load()?;
                for (id, g) in &added {
                    if let Some(warning) =
                        game::duplicate_exe_warning(&config, &g.exe_path, Some(id))
                    {
                        println!("  ⚠ [{}] {}", id, warning);
                    }
                }
            }
        }
        cli::Command::Sync { action } => {
            cli::sync_cli(&rt, action)?;
        }
        cli::Command::Scale { action } => {
            cli::scale_cli(&rt, action)?;
        }
    }

    Ok(())
}

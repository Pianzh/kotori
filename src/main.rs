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
         winit=warn,calloop=warn,sctk=warn,sctk_adwaita=warn,iced_wgpu=warn";
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
                for g in &games {
                    let id = game::generate_game_id(&g.name);
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
            }
        }
        cli::Command::Sync { action } => {
            sync_cli(&rt, action)?;
        }
        cli::Command::Scale { action } => {
            scale_cli(&rt, action)?;
        }
    }

    Ok(())
}

/// `kotori scale …`: change a running game's scaling right now.
///
/// Rescaling a game that is already running has no GUI button, so this is where
/// it happens. It needs no portal consent at all, which also makes it the way to
/// bisect "the window did not change": one command shows whether gamescope reacts.
fn scale_cli(rt: &tokio::runtime::Runtime, action: cli::ScaleCommand) -> anyhow::Result<()> {
    use cli::ScaleCommand;

    let socket = config::socket_path();
    daemon::ensure_running(&socket)?;

    match action {
        ScaleCommand::Status => {
            let status = call_daemon(rt, &socket, "daemon.status", None)?;
            print_scale_status(rt, &socket, &status)?;
        }
        ScaleCommand::Fsr { session_id } => {
            press(rt, &socket, "scale.toggle_fsr", session_id, None)?
        }
        ScaleCommand::Nis { session_id } => scale_action(rt, &socket, "toggle-nis", session_id)?,
        ScaleCommand::Integer { session_id } => {
            press(rt, &socket, "scale.toggle_integer", session_id, None)?
        }
        ScaleCommand::Linear { session_id } => {
            scale_action(rt, &socket, "toggle-linear", session_id)?
        }
        ScaleCommand::Sharpness { delta, session_id } => press(
            rt,
            &socket,
            "scale.adjust_sharpness",
            session_id,
            Some(delta),
        )?,
        ScaleCommand::Toggle { session_id } => {
            scale_action(rt, &socket, "toggle-scale", session_id)?
        }
        ScaleCommand::Up { session_id } => scale_action(rt, &socket, "scale-up", session_id)?,
        ScaleCommand::Down { session_id } => scale_action(rt, &socket, "scale-down", session_id)?,
        ScaleCommand::Reset { session_id } => scale_action(rt, &socket, "reset-scale", session_id)?,
        ScaleCommand::Fullscreen { session_id } => {
            scale_action(rt, &socket, "toggle-fullscreen", session_id)?
        }
    }

    Ok(())
}

/// `kotori scale up|down|reset|fullscreen`: one named action, by id.
///
/// One named action, by id — the same ids `ScaleAction` answers to, so the CLI
/// and anything else that asks for an action cannot drift apart.
fn scale_action(
    rt: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    action: &str,
    session_id: Option<String>,
) -> anyhow::Result<()> {
    let status = call_daemon(rt, socket, "daemon.status", None)?;
    let session = resolve_session(&status, session_id)?;
    let params = rpc::params([
        ("session_id", serde_json::json!(session)),
        ("action", serde_json::json!(action)),
    ]);
    let result = call_daemon(rt, socket, "scale.action", Some(params))?;
    report_action(&result, action);
    Ok(())
}

fn call_daemon(
    rt: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    method: &str,
    params: Option<serde_json::Map<String, serde_json::Value>>,
) -> anyhow::Result<serde_json::Value> {
    rt.block_on(rpc::call(socket, method, params))
        .map_err(anyhow::Error::msg)
}

/// Pick the session a scaling command applies to.
///
/// Only one game usually runs, so the id may be left out — but guessing between
/// two running games would rescale the wrong one, so that case lists them
/// instead.
fn resolve_session(status: &serde_json::Value, given: Option<String>) -> anyhow::Result<String> {
    if let Some(id) = given {
        return Ok(id);
    }
    let sessions = status["sessions"].as_array().cloned().unwrap_or_default();
    match sessions.as_slice() {
        [] => anyhow::bail!("没有正在运行的游戏（缩放只对 kotori 启动、且还在运行的游戏生效）"),
        [only] => Ok(only["session_id"].as_str().unwrap_or_default().to_string()),
        many => {
            let list: Vec<String> = many
                .iter()
                .map(|s| {
                    format!(
                        "  {} — {}",
                        s["session_id"].as_str().unwrap_or("?"),
                        s["game_id"].as_str().unwrap_or("?")
                    )
                })
                .collect();
            anyhow::bail!(
                "同时有多个游戏在运行，请指定 session_id：\n{}",
                list.join("\n")
            )
        }
    }
}

/// `kotori scale status`:有哪些会话在跑,以及 gamescope **此刻**在用什么缩放。
///
/// 会话列表来自 `daemon.status`;每一局的实时设置来自 `scale.get_status` —— 那是
/// gamescope 自己 Xwayland 根窗口上的属性,也就是 kotori 最后写下去的那一份。两个都
/// 列出来是因为它们会不一致(手动改过,或者 gamescope 自己的热键动过)。
fn print_scale_status(
    rt: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    status: &serde_json::Value,
) -> anyhow::Result<()> {
    let sessions = status["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("正在运行的游戏：无");
        return Ok(());
    }
    println!("正在运行的游戏：");
    for session in &sessions {
        let id = session["session_id"].as_str().unwrap_or("?");
        println!(
            "  {id} — {}（已运行 {}s）",
            session["game_id"].as_str().unwrap_or("?"),
            session["elapsed_secs"].as_u64().unwrap_or(0)
        );

        // 观测会话没有 gamescope 可问 —— 如实说,别拿档案里的值冒充"现在"。
        if session["gamescope_pid"].is_null() {
            println!("      仅观测（watch_only）：kotori 没有它的 gamescope 可调");
            continue;
        }
        let params = Some(rpc::params([("session_id", serde_json::json!(id))]));
        let live = call_daemon(rt, socket, "scale.get_status", params)?;
        match live["live"].as_object() {
            Some(live) => println!(
                "      现在：滤镜 {} / 缩放器 {} / 锐度 {}",
                live["filter"].as_str().unwrap_or("?"),
                live["scaler"].as_str().unwrap_or("?"),
                live["sharpness"].as_u64().unwrap_or(0)
            ),
            None => println!("      现在：读不到（Xwayland 还没就绪,或者已经退出）"),
        }
    }
    Ok(())
}

fn press(
    rt: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    method: &str,
    session_id: Option<String>,
    delta: Option<i32>,
) -> anyhow::Result<()> {
    let status = call_daemon(rt, socket, "daemon.status", None)?;
    let session = resolve_session(&status, session_id)?;

    let mut params = rpc::params([("session_id", serde_json::json!(session))]);
    if let Some(delta) = delta {
        params.insert("delta".into(), serde_json::json!(delta));
    }
    let result = call_daemon(rt, socket, method, Some(params))?;
    report_action(&result, method);
    Ok(())
}

/// Print what a scaling action did, for every path that runs one.
fn report_action(result: &serde_json::Value, fallback: &str) {
    let action = result
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or(fallback);
    let sessions = result
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|list| {
            list.iter()
                .map(|entry| {
                    let session = entry.get("session").and_then(|v| v.as_str()).unwrap_or("?");
                    let detail = entry.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                    if detail.is_empty() {
                        session.to_string()
                    } else {
                        format!("{session}（{detail}）")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!("已应用缩放动作 {action}（会话 {sessions}）");
    if let Some(failed) = result.get("failed").and_then(|f| f.as_array())
        && !failed.is_empty()
    {
        for entry in failed {
            println!(
                "  ⚠ 会话 {} 没生效：{}",
                entry.get("session").and_then(|s| s.as_str()).unwrap_or("?"),
                entry.get("error").and_then(|e| e.as_str()).unwrap_or("?")
            );
        }
    }
}

/// `kotori sync …`: everything goes through the daemon, like the GUI does.
///
/// The daemon owns the keyring handle and the config, so a second process
/// reading the config directly would be a second writer.
fn sync_cli(rt: &tokio::runtime::Runtime, action: cli::SyncCommand) -> anyhow::Result<()> {
    use cli::SyncCommand;

    let socket = config::socket_path();
    daemon::ensure_running(&socket)?;

    let (method, params) = match action {
        SyncCommand::Status => ("sync.status", rpc::params([])),
        SyncCommand::Test => ("sync.test", rpc::params([])),
        SyncCommand::Now { game_id } => (
            "sync.now",
            rpc::params(game_id.map(|id| ("id", serde_json::Value::String(id)))),
        ),
        SyncCommand::Versions { game_id } => (
            "sync.versions",
            rpc::params([("id", serde_json::Value::String(game_id))]),
        ),
        SyncCommand::Unlock => {
            let password = prompt_password("主密码: ")?;
            let result = rt.block_on(async {
                rpc::call(
                    &socket,
                    "sync.unlock",
                    Some(rpc::params([(
                        "password",
                        serde_json::Value::String(password),
                    )])),
                )
                .await
            });
            match result {
                Ok(_) => println!("已解锁"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        SyncCommand::MasterPassword => {
            println!(
                "主密码用来加密凭据文件（本机没有系统密钥环时用它）。\n\
                 它只由你保管：我们不会存它，忘了就打不开这个文件。"
            );
            let password = prompt_password("主密码（至少 8 位）: ")?;
            let again = prompt_password("再输一次: ")?;
            if password != again {
                eprintln!("两次输入不一样");
                std::process::exit(1);
            }
            let result = rt.block_on(async {
                rpc::call(
                    &socket,
                    "sync.set_master_password",
                    Some(rpc::params([
                        ("password", serde_json::Value::String(password)),
                        // The CLI asked twice already; that is the confirmation.
                        ("force", serde_json::Value::Bool(true)),
                    ])),
                )
                .await
            });
            match result {
                Ok(value) => println!(
                    "已加密保存 {} 条凭据到 {}",
                    value["count"].as_u64().unwrap_or(0),
                    value["path"].as_str().unwrap_or("?")
                ),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        SyncCommand::Lock => {
            let result = rt.block_on(async { rpc::call(&socket, "sync.lock", None).await });
            match result {
                Ok(_) => println!("已锁定"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        SyncCommand::Restore { game_id, version } => {
            let mut params = rpc::params([("id", serde_json::Value::String(game_id))]);
            if let Some(version) = version {
                params.insert("version".into(), serde_json::Value::String(version));
            }
            ("sync.restore", params)
        }
    };

    let result = rt.block_on(async { rpc::call(&socket, method, Some(params)).await });
    match result {
        Ok(value) => {
            print_sync_result(method, &value);
            Ok(())
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Read a password without echoing it.
///
/// It never becomes a command-line argument: `ps` is world-readable, and the
/// shell history outlives the session.
#[cfg(unix)]
fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    use nix::sys::termios::{self, LocalFlags, SetArg};
    use std::io::{BufRead, Write};

    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    let stdin = std::io::stdin();
    let original = termios::tcgetattr(&stdin).ok();
    if let Some(original) = &original {
        let mut quiet = original.clone();
        quiet.local_flags.remove(LocalFlags::ECHO);
        let _ = termios::tcsetattr(&stdin, SetArg::TCSANOW, &quiet);
    }

    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);

    if let Some(original) = &original {
        let _ = termios::tcsetattr(&stdin, SetArg::TCSANOW, original);
    }
    eprintln!();
    read?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Windows 上没有 termios 可以关回显。
///
/// ⚠ 这是**已知的削弱**:这里的密码会原样显示在屏幕上。CLI 这个入口在 Windows 上
/// 本来就很少用(桌面用户走 GUI),为它引一个新依赖不划算 —— 真需要时再换
/// `rpassword`(它在 Windows 上走 SetConsoleMode)。
#[cfg(not(unix))]
fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    use std::io::{BufRead, Write};

    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;

    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Sync output is meant to be read by a human, not piped into `jq` — so it is
/// summarised rather than dumped. `--json` can come later if it is ever needed.
fn print_sync_result(method: &str, value: &serde_json::Value) {
    match method {
        "sync.status" => {
            let on = |v: &serde_json::Value| v.as_bool().unwrap_or(false);
            println!(
                "云同步: {}",
                if on(&value["enabled"]) {
                    "已启用"
                } else {
                    "关闭"
                }
            );
            println!("远端: {}", value["remote"].as_str().unwrap_or("-"));
            println!(
                "rclone: {}",
                value["rclone"]
                    .as_str()
                    .unwrap_or("未安装（Arch: pacman -S rclone）")
            );
            println!(
                "密钥环: {}",
                value["keyring"]["backend"].as_str().unwrap_or("-")
            );
            // Which of the (single) credential slots are filled — the values
            // themselves never leave the keyring.
            let saved = |account: &str| {
                value["secrets"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|a| a.as_str() == Some(account)))
            };
            let mark = |account: &str| if saved(account) { "✓" } else { "✗" };
            println!(
                "凭据: keyID {} applicationKey {}",
                mark("b2-key-id"),
                mark("b2-app-key")
            );
            if let Some(problem) = value["problem"].as_str() {
                println!("待解决: {problem}");
            }
            if let Some(games) = value["games"].as_array() {
                println!("游戏（{} 个）:", games.len());
                for game in games {
                    let last = &game["last"];
                    let when = if last.is_null() {
                        "还没同步过".to_string()
                    } else {
                        format!(
                            "{} {} {}",
                            last["at"].as_str().unwrap_or("-"),
                            last["action"].as_str().unwrap_or(""),
                            last["detail"].as_str().unwrap_or("")
                        )
                    };
                    println!(
                        "  [{}] {} — {} 个存档位置，{when}",
                        game["id"].as_str().unwrap_or("?"),
                        game["name"].as_str().unwrap_or("?"),
                        game["locations"].as_u64().unwrap_or(0)
                    );
                    if let Some(problem) = game["location_problem"].as_str() {
                        println!("      ⚠ {problem}");
                    }
                }
            }
        }
        "sync.test" => println!(
            "连接正常: {}",
            value["remote"].as_str().unwrap_or("(未知远端)")
        ),
        "sync.versions" => {
            let versions = value["versions"].as_array().cloned().unwrap_or_default();
            if versions.is_empty() {
                println!("云端还没有这个游戏的存档版本");
            } else {
                println!("版本（最旧在前）:");
                for version in versions {
                    println!("  {}", version.as_str().unwrap_or("-"));
                }
            }
        }
        "sync.now" | "sync.restore" => {
            // A single game comes back under `game`; a bulk run under `games`.
            let games: Vec<&serde_json::Value> = match (
                value["games"].as_array(),
                value.get("game").filter(|g| !g.is_null()),
            ) {
                (Some(games), _) => games.iter().collect(),
                (None, Some(game)) => vec![game],
                _ => Vec::new(),
            };
            let mut failed = false;
            for game in games {
                let name = game["name"].as_str().unwrap_or("?");
                let id = game["game_id"].as_str().unwrap_or("?");
                if let Some(error) = game["error"].as_str() {
                    failed = true;
                    println!("✗ [{id}] {name}: {error}");
                } else {
                    println!("✓ [{id}] {name}");
                }
                for location in game["locations"].as_array().into_iter().flatten() {
                    println!(
                        "    {} {} — {}",
                        location["action"].as_str().unwrap_or("-"),
                        location["configured"].as_str().unwrap_or("-"),
                        location["detail"].as_str().unwrap_or("")
                    );
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        _ => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    }
}

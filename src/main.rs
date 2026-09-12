mod cli;
mod config;
mod daemon;
mod display;
mod game;
mod hotkeys;
mod process;
mod rpc;
mod scale;
mod secrets;
mod sync;
mod ui;
mod util;
mod wine;

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

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

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
            file: matches!(&cli.command, cli::Command::Ui)
                .then(open_ui_log)
                .flatten()
                .map(|file| std::sync::Arc::new(std::sync::Mutex::new(file))),
        })
        .init();

    let cli = cli::Cli::parse();

    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        cli::Command::Daemon => {
            tracing::info!("Starting daemon mode");
            rt.block_on(async { daemon::run().await })?;
        }
        cli::Command::Ui => {
            tracing::info!("Starting UI mode");
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
/// The buttons the user actually presses are the portal's global shortcuts; this
/// is the same action without the key, so it works from a script or a terminal
/// (and needs no portal consent at all). It is also what makes "the hotkey did
/// nothing" bisectable: one command separates "the trigger never fired" from
/// "gamescope did not react".
fn scale_cli(rt: &tokio::runtime::Runtime, action: cli::ScaleCommand) -> anyhow::Result<()> {
    use cli::ScaleCommand;

    let socket = config::socket_path();
    daemon::ensure_running(&socket)?;

    match action {
        ScaleCommand::Status => {
            let status = call_daemon(rt, &socket, "daemon.status", None)?;
            print_scale_status(&status);
        }
        ScaleCommand::Hotkeys { wait } => ask_for_hotkeys(rt, &socket, wait)?,
        ScaleCommand::Fsr { session_id } => {
            press(rt, &socket, "scale.toggle_fsr", session_id, None)?
        }
        ScaleCommand::Integer { session_id } => {
            press(rt, &socket, "scale.toggle_integer", session_id, None)?
        }
        ScaleCommand::Sharpness { delta, session_id } => press(
            rt,
            &socket,
            "scale.adjust_sharpness",
            session_id,
            Some(delta),
        )?,
    }

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

fn print_scale_status(status: &serde_json::Value) {
    let hotkeys = &status["hotkeys"];
    let state = match (
        hotkeys["ready"].as_bool(),
        hotkeys["error"].as_str(),
        hotkeys["requested"].as_bool(),
    ) {
        (Some(true), ..) => "已就绪".to_string(),
        (_, Some(err), _) => format!("不可用：{err}"),
        (_, _, Some(true)) => "正在等你在弹窗里确认".to_string(),
        _ => "还没申请（启动一次游戏，或跑 kotori scale hotkeys）".to_string(),
    };
    println!("运行时缩放热键：{state}");
    print_unbound(hotkeys);

    let sessions = status["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("正在运行的游戏：无");
    } else {
        println!("正在运行的游戏：");
        for s in &sessions {
            println!(
                "  {} — {}（已运行 {}s）",
                s["session_id"].as_str().unwrap_or("?"),
                s["game_id"].as_str().unwrap_or("?"),
                s["elapsed_secs"].as_u64().unwrap_or(0)
            );
        }
    }
}

/// A shortcut the desktop granted without a key behind it looks exactly like
/// success and does nothing. Say which ones, and where to give them a key.
fn print_unbound(hotkeys: &serde_json::Value) {
    let unbound: Vec<&str> = hotkeys["unbound"]
        .as_array()
        .map(|list| list.iter().filter_map(|id| id.as_str()).collect())
        .unwrap_or_default();
    if unbound.is_empty() {
        return;
    }
    println!(
        "  ⚠ 这些动作还没有按键，按了不会有反应：{}",
        unbound.join(", ")
    );
    if let Some(hint) = hotkeys["assign_hint"].as_str() {
        println!("    {hint}");
    }
}

/// Ask the portal for the hotkeys, and optionally wait for the user to approve
/// the dialog.
///
/// Without `--wait` this returns while the dialog is still up: the request is
/// asynchronous on the daemon side (it must never block a game launch), so the
/// outcome can only be observed by polling `daemon.status`.
fn ask_for_hotkeys(
    rt: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    wait: u64,
) -> anyhow::Result<()> {
    let value = call_daemon(rt, socket, "scale.hotkeys", None)?;
    if value["ready"].as_bool() == Some(true) {
        println!("运行时缩放热键已就绪");
        print_unbound(&value);
        return Ok(());
    }
    if let Some(err) = value["error"].as_str() {
        println!("运行时缩放热键不可用：{err}");
        return Ok(());
    }
    if value["requested"].as_bool() != Some(true) {
        println!("没能申请运行时缩放热键（守护进程没有回应，看看 daemon 日志）");
        return Ok(());
    }
    println!("已向桌面门户申请授权，请在弹窗里确认（弹窗会列出每个快捷键）");
    if wait == 0 {
        return Ok(());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let status = call_daemon(rt, socket, "daemon.status", None)?;
        let hotkeys = &status["hotkeys"];
        if hotkeys["ready"].as_bool() == Some(true) {
            println!("运行时缩放热键已就绪");
            print_unbound(hotkeys);
            return Ok(());
        }
        if let Some(err) = hotkeys["error"].as_str() {
            println!("申请失败：{err}");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            println!("等了 {wait}s 还没确认，先不等了（弹窗可能还开着）");
            return Ok(());
        }
    }
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
    let action = result
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or(method);
    let sessions = result
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|s| s.as_str())
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
    Ok(())
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
                "凭据: keyID {} applicationKey {} 同步密码 {}",
                mark("b2-key-id"),
                mark("b2-app-key"),
                mark("sync-password")
            );
            if let Some(problem) = value["problem"].as_str() {
                println!("待解决: {problem}");
            }
            println!(
                "取回密码: {}",
                value["password_hint"].as_str().unwrap_or("-")
            );
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
                println!("云端还没有这个游戏的快照");
            } else {
                println!("快照（最旧在前）:");
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

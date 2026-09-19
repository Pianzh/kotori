//! `kotori sync …` 的 CLI 侧:全部经 daemon(与 GUI 同一条路),把输出说成人话。
//!
//! 从 `main.rs` 整体搬来(纯移动):密码的两种平台读法与人类可读的结果打印都
//! 属于这一族 —— daemon 拥有密钥环句柄与配置,直接读配置的第二个进程就是
//! 第二个写者,所以这条 CLI 路径只发 RPC。

use super::SyncCommand;
use crate::{config, daemon, rpc};

pub(crate) fn sync_cli(rt: &tokio::runtime::Runtime, action: SyncCommand) -> anyhow::Result<()> {
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
            // 字形与界面同一条规矩:用 `√` / `×`,不用雅黑缺字形的 `✓` / `✗`
            // (见 `ui::parse::sync::credentials_label`)。
            let mark = |account: &str| if saved(account) { "√" } else { "×" };
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
                    println!("× [{id}] {name}: {error}");
                } else {
                    println!("√ [{id}] {name}");
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

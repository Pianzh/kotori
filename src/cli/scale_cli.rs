//! `kotori scale …` 的 CLI 侧:把动作发给 daemon、把结果说成人话。
//!
//! 从 `main.rs` 整体搬来(纯移动):main 只留入口与分发,这一族函数与 sync 的
//! CLI 各占一个文件 —— 它们只跟 daemon 的 JSON 打交道,彼此从不一起改。

use super::ScaleCommand;
use crate::{config, daemon, rpc};

pub(crate) fn scale_cli(rt: &tokio::runtime::Runtime, action: ScaleCommand) -> anyhow::Result<()> {
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

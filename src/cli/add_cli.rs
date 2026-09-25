//! `kotori add …` 的 CLI 侧:优先交给 daemon,daemon 不在才自己写文件。
//!
//! 从前这条命令直接 `load()`/`save()` 配置,CLI 因此成了**第二个写者**(BUG-16):
//! 它和 daemon 的自动保存在同一毫秒落地时,谁后写谁赢,先写的那一笔就没了 ——
//! 丢的可能是一个刚加的游戏,也可能是用户刚在设置页改的一项。现在分两层:
//!
//!   1. **daemon 在跑** —— 发 `game.add` 让它去加。单一写者、内存与磁盘同一步
//!      更新,GUI 不用刷新就能看到新游戏;
//!   2. **daemon 没跑** —— 拿住配置文件的跨进程锁,自己"读-改-写",写完再尽力
//!      让(可能在我们探活之后才起来的)daemon 重读一次。
//!
//! 两条路都不要求用户关心 daemon 在不在:`kotori add` 照旧,不加参数、不等、
//! 也不会为了加一个游戏顺手把一个常驻进程拉起来。

use std::path::Path;

use serde_json::json;

use crate::{config, game, rpc};

/// 打印用的一条新增:`(id, 名字, exe 路径)`。
type Added = (String, String, String);
/// 打印用的一条提醒:`(id, 文案)`。
type Warning = (String, String);

pub(crate) fn add_cli(rt: &tokio::runtime::Runtime, directory: &Path) -> anyhow::Result<()> {
    let socket = config::socket_path();

    // ① 有 daemon:它就是那个唯一写者。
    if rt.block_on(rpc::is_running(&socket)) {
        let reply = rt
            .block_on(rpc::call(
                &socket,
                "game.add",
                Some(rpc::params([(
                    "directory",
                    json!(directory.display().to_string()),
                )])),
            ))
            .map_err(anyhow::Error::msg)?;
        let (added, warnings) = split_reply(&reply);
        report(directory, &added, &warnings);
        return Ok(());
    }

    // ② 没有 daemon:也就没有别人在写配置,但**可能还有别的 CLI** ——
    // `add_from_dir` 自己拿住那把跨进程锁,把"读-改-写"整个圈起来。
    let added = game::add_from_dir(directory)?;
    // 提醒要用**加完之后**那份配置算(它得看得见刚加进去的那几条)。
    let current = config::load()?;
    let warnings = added
        .iter()
        .filter_map(|(id, entry)| {
            game::duplicate_exe_warning(&current, &entry.exe_path, Some(id))
                .map(|message| (id.clone(), message))
        })
        .collect::<Vec<_>>();
    let added = added
        .iter()
        .map(|(id, entry)| {
            (
                id.clone(),
                entry.name.clone(),
                entry.exe_path.display().to_string(),
            )
        })
        .collect::<Vec<_>>();

    // ③ 就在探活那一瞬间之后,daemon 可能起来了,而它读到的是我们写之前的那一份。
    // 这一发是**尽力而为**:连不上(还是没起来)或者它不认这条请求,都不该让一条
    // 已经成功的 `add` 变成失败。
    let _ = rt.block_on(rpc::call(&socket, "config.reload", None));

    report(directory, &added, &warnings);
    Ok(())
}

/// 输出的形状**一个字都没变** —— 从前这段在 `main.rs` 里,测试与用户的习惯都
/// 建立在它上面。
fn report(directory: &Path, added: &[Added], warnings: &[Warning]) {
    if added.is_empty() {
        println!("No new games added from {}", directory.display());
        return;
    }
    println!("Added {} game(s):", added.len());
    for (id, name, exe_path) in added {
        println!("  [{id}] {name} -> {exe_path}");
    }
    for (id, message) in warnings {
        println!("  ⚠ [{id}] {message}");
    }
}

fn split_reply(reply: &serde_json::Value) -> (Vec<Added>, Vec<Warning>) {
    let added = reply["added"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| (text(item, "id"), text(item, "name"), text(item, "exe_path")))
                .collect()
        })
        .unwrap_or_default();
    let warnings = reply["warnings"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| (text(item, "id"), text(item, "message")))
                .collect()
        })
        .unwrap_or_default();
    (added, warnings)
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .unwrap_or_default()
        .to_string()
}

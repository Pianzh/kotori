//! 云同步向 daemon 发的一切请求:状态、设置、凭据、立即同步、恢复、云端清单。
//!
//! 从 `tasks.rs` 拆出来:那个文件是"所有 RPC"的长列表,而云同步这一族(尤其是凭据
//! 与那三把钥匙)本来就是一整块 —— 拆开之后两边都读得完。这里改的仍然是同一层:
//! `use super::*` 拿到的东西与在父模块里一模一样。

use super::*;

/// Read the cloud-sync status. Secrets are never returned by the daemon, so
/// this can be held in the UI without any caution.
pub(in crate::ui) async fn load_sync_status() -> Result<SyncStatus, String> {
    let socket = crate::config::socket_path();
    let value = crate::rpc::call(&socket, "sync.status", None).await?;
    parse_sync_status(&value)
}

/// Persist the sync settings. The daemon validates and may refuse (an
/// impossible prefix, say), so its message is surfaced verbatim.
pub(in crate::ui) async fn save_sync_settings(socket: &Path, patch: Value) -> Result<bool, String> {
    let params = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "内部错误：设置补丁不是对象".to_string())?;
    let value = crate::rpc::call(socket, "sync.set_settings", Some(params)).await?;
    Ok(engine_changed(&value))
}

/// 只提交"用哪个引擎"这一个字段。
///
/// 引擎是个二选一的开关，点下去就该生效 —— 不该跟 bucket 那些字段一起等「保存设置」。
/// `sync.set_settings` 是**按字段合并**的（`SettingsPatch` 全是 `Option`），所以这里
/// 只发 engine 一个键，用户还没保存的其它编辑一个都不会被带上，也不会被当成已保存。
pub(in crate::ui) async fn save_sync_engine(socket: &Path, engine: &str) -> Result<bool, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_settings",
        Some(crate::rpc::params([(
            "engine",
            Value::String(engine.to_string()),
        )])),
    )
    .await?;
    Ok(engine_changed(&value))
}

/// daemon 在 `sync.set_settings` 的回包里说"这次换引擎了"。
///
/// 值得单独一个函数，是因为它带的是**一句必须说出口的警告**：换了引擎之后，另一个
/// 引擎传上去的版本不会显示出来（数据还在桶里，只是这边读不出来），而这件事**不会
/// 报错**。丢掉它，用户就只能自己发现"我的存档怎么不见了"—— 这一条从前是被
/// `Ok(())` 整个吞掉的。
fn engine_changed(value: &Value) -> bool {
    value
        .get("engine_changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub(in crate::ui) async fn save_sync_credentials(
    socket: &Path,
    key_id: &str,
    app_key: &str,
) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.set_credentials",
        Some(crate::rpc::params([
            ("key_id", Value::String(key_id.to_string())),
            ("app_key", Value::String(app_key.to_string())),
        ])),
    )
    .await?;
    Ok(())
}

/// 设置（或清除）kopia 仓库密码。留空 = 清除 = 回到默认的 `kotori`。
///
/// 返回 `true` 表示"现在用的是默认密码"——这句话必须让用户看见：默认密码意味着
/// 任何拿到桶的人都能解开仓库。
pub(in crate::ui) async fn save_kopia_password(
    socket: &Path,
    password: &str,
) -> Result<bool, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_kopia_password",
        Some(crate::rpc::params([(
            "password",
            Value::String(password.to_string()),
        )])),
    )
    .await?;
    Ok(value
        .get("using_default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// Unlock the master-password file. The password goes over IPC to our own
/// daemon and is never written anywhere.
pub(in crate::ui) async fn unlock_credentials(socket: &Path, password: &str) -> Result<(), String> {
    crate::rpc::call(
        socket,
        "sync.unlock",
        Some(crate::rpc::params([(
            "password",
            Value::String(password.to_string()),
        )])),
    )
    .await?;
    Ok(())
}

/// 锁上凭据文件:派生出来的密钥从内存里丢掉,再想看凭据就得重新输主密码。
pub(in crate::ui) async fn lock_credentials(socket: &Path) -> Result<(), String> {
    crate::rpc::call(socket, "sync.lock", None).await?;
    Ok(())
}

/// 删掉主密码凭据文件。里面的凭据一起消失 —— 忘了主密码时这是唯一的出路,
/// 所以它不需要先解锁(见 `rpc_sync_clear_master_password`)。
pub(in crate::ui) async fn clear_master_file(socket: &Path) -> Result<(), String> {
    crate::rpc::call(socket, "sync.clear_master_password", None).await?;
    Ok(())
}

/// Seal the current credentials into a master-password file, and say where it
/// landed.
pub(in crate::ui) async fn set_master_password(
    socket: &Path,
    password: &str,
) -> Result<String, String> {
    let value = crate::rpc::call(
        socket,
        "sync.set_master_password",
        Some(crate::rpc::params([
            ("password", Value::String(password.to_string())),
            // The UI asked for the password in a dedicated field; that is the
            // confirmation.
            ("force", Value::Bool(true)),
        ])),
    )
    .await?;
    Ok(str_field(&value, "path"))
}

pub(in crate::ui) async fn sync_test(socket: &Path) -> Result<String, String> {
    let value = crate::rpc::call(socket, "sync.test", None).await?;
    Ok(str_field(&value, "remote"))
}

/// 扫一遍云端，拿回配对表（`sync.pairing`）。
///
/// ⚠ 这是**唯一**一条会读云端身份的路（kopia 那边读一次 = 一次 `restore`），所以它
/// 只挂在「扫描云端」那个按钮上，不跟着状态刷新跑。
pub(in crate::ui) async fn sync_scan_cloud(socket: &Path) -> Result<Vec<PairingRow>, String> {
    let value = crate::rpc::call(socket, "sync.pairing", None).await?;
    parse_pairing(&value)
}

/// 云端有哪几款、各几版（`sync.cloud_games`）。
///
/// 与配对扫描分开：这一条**不读身份卡**（kopia 那边是一条轻快的列出快照，不是
/// `restore`），所以「云端存档」这一块可以随用户按刷新就走，不必等他扫配对。
pub(in crate::ui) async fn sync_cloud_games(socket: &Path) -> Result<Vec<CloudSaveRow>, String> {
    let value = crate::rpc::call(socket, "sync.cloud_games", None).await?;
    parse_cloud_games(&value)
}

/// 云端某一款有哪几版（`sync.cloud_versions`）。
///
/// ⚠ 参数是**云端落点**（`sync.cloud_games` 给的 key），不是本机游戏 id ——
/// 云端有而本机没有的游戏没有 id 可用。
pub(in crate::ui) async fn sync_cloud_versions(
    socket: &Path,
    cloud_key: String,
) -> Result<Vec<String>, String> {
    let params = crate::rpc::params([("key", Value::String(cloud_key))]);
    let value = crate::rpc::call(socket, "sync.cloud_versions", Some(params)).await?;
    parse_cloud_versions(&value)
}

/// 把本机这一款绑到云端那个身份上，然后重新扫一遍（表要反映刚做的决定）。
pub(in crate::ui) async fn sync_pair(
    socket: &Path,
    local_id: String,
    cloud_key: String,
    cloud_id: String,
) -> Result<Vec<PairingRow>, String> {
    let params = crate::rpc::params([
        ("id", Value::String(local_id)),
        ("cloud_key", Value::String(cloud_key)),
        ("cloud_id", Value::String(cloud_id)),
    ]);
    crate::rpc::call(socket, "sync.pair", Some(params)).await?;
    sync_scan_cloud(socket).await
}

/// 「不是同一款」：撤掉绑定并记住，然后重新扫一遍。
pub(in crate::ui) async fn sync_reject_pairing(
    socket: &Path,
    local_id: String,
    cloud_id: String,
) -> Result<Vec<PairingRow>, String> {
    let params = crate::rpc::params([
        ("id", Value::String(local_id)),
        ("cloud_id", Value::String(cloud_id)),
    ]);
    crate::rpc::call(socket, "sync.reject", Some(params)).await?;
    sync_scan_cloud(socket).await
}

/// Upload now, and turn the daemon's per-location report into one line.
pub(in crate::ui) async fn sync_now(
    socket: &Path,
    game_id: Option<String>,
) -> Result<String, String> {
    let params = crate::rpc::params(game_id.map(|id| ("id", Value::String(id))));
    let value = crate::rpc::call(socket, "sync.now", Some(params)).await?;
    Ok(describe_sync_outcome(&value))
}

pub(in crate::ui) async fn sync_restore(
    socket: &Path,
    game_id: &str,
    version: Option<&str>,
) -> Result<String, String> {
    let mut params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    if let Some(version) = version {
        params.insert("version".into(), Value::String(version.to_string()));
    }
    let value = crate::rpc::call(socket, "sync.restore", Some(params)).await?;
    Ok(describe_sync_outcome(&value["game"]))
}

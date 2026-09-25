//! 「云同步」页:连接与保留、凭据。

use super::*;

pub(super) fn push_sync(ui: &mut Ui) {
    let app = &ui.app;
    // 这一族住在一个 Slint 全局里(见 `slint/state/sync-board.slint`):页面与 Rust 都直接
    // 读写它,根窗口不再替它转发 —— 它曾经是 `app.slint` 里最长的那一段。
    let f = ui.window.global::<SyncBoard>();
    let form = &app.sync_form;
    let status = app.sync_status.as_ref();

    // ── 可编辑的一半 ──
    push_bool(f.get_enabled(), form.enabled, |v| f.set_enabled(v));
    push_str(f.get_bucket(), &form.bucket, |v| f.set_bucket(v));
    push_str(f.get_prefix(), &form.prefix, |v| f.set_prefix(v));
    push_str(f.get_endpoint(), &form.endpoint, |v| f.set_endpoint(v));
    push_str(f.get_keep_versions(), &form.keep_versions, |v| {
        f.set_keep_versions(v)
    });
    // 两个"程序位置"：用户**填的**那一份（空 = 由 kotori 自己找）。它们和 bucket 一样
    // 属于表单；"当前生效的路径"是另一回事，由下面只读区推（见 `sync-rclone`）。
    push_str(f.get_rclone_input(), &form.rclone_binary, |v| {
        f.set_rclone_input(v)
    });
    push_str(f.get_kopia_input(), &form.kopia_binary, |v| {
        f.set_kopia_input(v)
    });
    push_str(f.get_key_id(), &form.key_id, |v| f.set_key_id(v));
    push_str(f.get_app_key(), &form.app_key, |v| f.set_app_key(v));
    push_str(f.get_master_password(), &form.master_password, |v| {
        f.set_master_password(v)
    });
    push_str(f.get_engine(), &form.engine, |v| f.set_engine(v));
    push_str(f.get_kopia_password(), &form.kopia_password, |v| {
        f.set_kopia_password(v)
    });
    push_bool(f.get_connection_revealed(), form.connection_revealed, |v| {
        f.set_connection_revealed(v)
    });
    push_bool(f.get_busy(), form.busy, |v| f.set_busy(v));
    push_bool(
        f.get_confirm_master_delete(),
        form.confirm_master_delete,
        |v| f.set_confirm_master_delete(v),
    );

    let message = form.msg.clone().unwrap_or_default();
    let ok = !message.contains("失败") && !message.contains("不一样") && !message.contains("请先");
    push_str(f.get_message(), &message, |v| f.set_message(v));
    push_bool(f.get_message_ok(), ok, |v| f.set_message_ok(v));

    // ── 只读的一半 ──
    push_bool(f.get_loaded(), form.loaded, |v| f.set_loaded(v));
    let (has_key_id, has_app_key) = match status {
        Some(status) => (
            status.has_secret("b2-key-id"),
            status.has_secret("b2-app-key"),
        ),
        None => (false, false),
    };
    let empty = SyncStatus::default();
    let status = status.unwrap_or(&empty);
    push_str(f.get_remote(), &status.remote, |v| f.set_remote(v));
    push_str(
        f.get_rclone(),
        status.rclone.as_deref().unwrap_or_default(),
        |v| f.set_rclone(v),
    );
    push_str(
        f.get_kopia_binary(),
        status.kopia.as_deref().unwrap_or_default(),
        |v| f.set_kopia_binary(v),
    );
    // 引擎只推一次(上面表单那一处):它既是可编辑项、又是状态显示项,
    // 推两次会让用户刚点的选择被随后的 status 覆盖。
    push_str(f.get_kopia_prefix(), &status.kopia_prefix, |v| {
        f.set_kopia_prefix(v)
    });
    // 密码状态只说"是不是默认",绝不说值 —— 值从来没离开过凭据库。
    let kopia_password_set = status.has_secret("kopia-password");
    push_bool(f.get_kopia_using_default(), !kopia_password_set, |v| {
        f.set_kopia_using_default(v)
    });
    push_str(
        f.get_kopia_password_state(),
        if kopia_password_set {
            "已自己设置(存在凭据库里)"
        } else {
            "默认的 kotori"
        },
        |v| f.set_kopia_password_state(v),
    );
    push_str(f.get_keyring(), &status.keyring, |v| f.set_keyring(v));
    push_bool(f.get_ephemeral(), status.ephemeral, |v| f.set_ephemeral(v));
    push_str(f.get_keyring_hint(), crate::secrets::keyring_hint(), |v| {
        f.set_keyring_hint(v)
    });
    push_str(
        f.get_problem(),
        status.problem.as_deref().unwrap_or_default(),
        |v| f.set_problem(v),
    );
    push_bool(f.get_ready(), status.ready, |v| f.set_ready(v));
    push_int(f.get_store_kind(), status.store().index(), |v| {
        f.set_store_kind(v)
    });
    // 说"存到哪"/"存在哪"都用这一级自己的名字:没有密钥环的机器上凭据只在内存里,
    // 文案写成"密钥环"就是在骗用户。
    push_str(f.get_store_name(), status.store().name(), |v| {
        f.set_store_name(v)
    });
    push_str(f.get_store_path(), &status.store_path, |v| {
        f.set_store_path(v)
    });
    push_str(f.get_master_file(), &status.master_file, |v| {
        f.set_master_file(v)
    });
    push_bool(f.get_store_locked(), status.store_locked, |v| {
        f.set_store_locked(v)
    });
    push_bool(
        f.get_can_delete_credentials(),
        has_key_id || has_app_key,
        |v| f.set_can_delete_credentials(v),
    );
    push_str(
        f.get_credentials_label(),
        &credentials_label(has_key_id, has_app_key, status.store().name()),
        |v| f.set_credentials_label(v),
    );
    push_str(
        f.get_master_hint(),
        &format!("至少 {} 位,自己记得住就行", app.min_master_password()),
        |v| f.set_master_hint(v),
    );
}

/// `sync.status` reports the credential store by name; the page wants a number
/// (it decides which of the three blocks to draw). The mapping itself lives on
/// [`CredentialStore`] — the wording and the index must not drift apart.
/// 启动前那一问：开不开、问的是哪一款、以及那三行说明。
///
/// 状态在一个 Slint 全局里（见 `widgets/sync-ask.slint`）：它不属于任何一页，哪个
/// 页面点的「启动」都可能触发它。
pub(super) fn push_sync_ask(ui: &mut Ui) {
    let ask = ui.window.global::<SyncAskState>();
    let game = ui
        .app
        .sync_ask
        .as_ref()
        .and_then(|id| ui.app.games.iter().find(|game| &game.id == id));
    // 「改配对…」把它让位给云端清单时先收起来（`sync_ask` 还留着，挑完要用它启动）。
    let open = game.is_some() && !ui.app.sync_ask_hidden;
    push_bool(ask.get_open(), open, |v| ask.set_open(v));
    let name = game.map(|game| game.name.clone()).unwrap_or_default();
    push_str(ask.get_game_name(), &name, |v| ask.set_game_name(v));
    // 一句话说清现状：有像的就说有像的，没有就说没有 —— 用户 2026-09-24：文案要
    // "简短通俗、不要括号、不要废话"。云端那一条叫什么画在下面那块卡片里，名字与摘要
    // 由 `identity_label` 生成。
    let message = if game.is_none() {
        ""
    } else if ui.app.sync_ask_cloud.is_some() {
        "云端有一条像的，但不敢替你定。"
    } else {
        "云端没有对得上的。"
    };
    push_str(ask.get_message(), message, |v| ask.set_message(v));
    // "疑似找到的那一条"：名字与摘要走 `identity_label`（与单游戏页「当前绑定」**同一个
    // 函数**）；没有就是"完全没找到"，界面照实说、让用户自己挑。
    push_bool(
        ask.get_cloud_found(),
        ui.app.sync_ask_cloud.is_some(),
        |v| ask.set_cloud_found(v),
    );
    let (cloud_name, cloud_summary) = match &ui.app.sync_ask_cloud {
        Some(cloud) => {
            let label = identity_label(
                &cloud.cloud_id,
                &cloud.cloud_key,
                &cloud.name,
                cloud.versions,
                &cloud.latest,
                cloud.size,
            );
            (label.name, label.summary)
        }
        None => (String::new(), String::new()),
    };
    push_str(ask.get_cloud_name(), &cloud_name, |v| ask.set_cloud_name(v));
    push_str(ask.get_cloud_summary(), &cloud_summary, |v| {
        ask.set_cloud_summary(v)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_store_gets_its_own_index_and_its_own_name() {
        assert_eq!(CredentialStore::System.index(), 0);
        assert_eq!(CredentialStore::File.index(), 1);
        assert_eq!(CredentialStore::Session.index(), 2);
        assert_eq!(CredentialStore::Plain.index(), 3);
        // 明文是默认落点:认不出来就会被当成密钥环,又变回"骗用户"。
        assert_eq!(
            CredentialStore::from_wire("plain-file"),
            CredentialStore::Plain
        );
        assert_eq!(CredentialStore::default(), CredentialStore::System);
        assert_eq!(CredentialStore::Plain.name(), "明文凭据文件");
        let note = CredentialStore::Plain.saved_note("凭据");
        assert!(note.contains("0600"), "{note}");
        assert!(!note.contains("密钥环"), "{note}");
        assert_eq!(
            CredentialStore::from_wire("system"),
            CredentialStore::System
        );
        assert_eq!(
            CredentialStore::from_wire("encrypted-file"),
            CredentialStore::File
        );
        assert_eq!(
            CredentialStore::from_wire("session-only"),
            CredentialStore::Session
        );
        // 不认识的答复按最坏情况算:当作系统密钥环,不吓唬用户。
        assert_eq!(CredentialStore::from_wire(""), CredentialStore::System);

        // 三级各有各的说法,不能都叫"密钥环"。
        let names = [
            CredentialStore::System.name(),
            CredentialStore::File.name(),
            CredentialStore::Session.name(),
        ];
        assert!(names.iter().all(|name| !name.is_empty()));
        assert_eq!(names[0], "系统密钥环");
        assert!(!names[2].contains("密钥环"), "内存不是密钥环:{}", names[2]);
    }
}

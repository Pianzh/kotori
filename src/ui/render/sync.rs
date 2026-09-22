//! 「云同步」页:连接与保留、凭据。

use super::*;

pub(super) fn push_sync(ui: &mut Ui) {
    let app = &ui.app;
    let w = &ui.window;
    let form = &app.sync_form;
    let status = app.sync_status.as_ref();

    // ── 可编辑的一半 ──
    push_bool(w.get_sync_enabled(), form.enabled, |v| {
        w.set_sync_enabled(v)
    });
    push_str(w.get_sync_bucket(), &form.bucket, |v| w.set_sync_bucket(v));
    push_str(w.get_sync_prefix(), &form.prefix, |v| w.set_sync_prefix(v));
    push_str(w.get_sync_endpoint(), &form.endpoint, |v| {
        w.set_sync_endpoint(v)
    });
    push_str(w.get_sync_keep_versions(), &form.keep_versions, |v| {
        w.set_sync_keep_versions(v)
    });
    // 两个"程序位置"：用户**填的**那一份（空 = 由 kotori 自己找）。它们和 bucket 一样
    // 属于表单；"当前生效的路径"是另一回事，由下面只读区推（见 `sync-rclone`）。
    push_str(w.get_sync_rclone_input(), &form.rclone_binary, |v| {
        w.set_sync_rclone_input(v)
    });
    push_str(w.get_sync_kopia_input(), &form.kopia_binary, |v| {
        w.set_sync_kopia_input(v)
    });
    push_str(w.get_sync_key_id(), &form.key_id, |v| w.set_sync_key_id(v));
    push_str(w.get_sync_app_key(), &form.app_key, |v| {
        w.set_sync_app_key(v)
    });
    push_str(w.get_sync_master_password(), &form.master_password, |v| {
        w.set_sync_master_password(v)
    });
    push_str(w.get_sync_engine(), &form.engine, |v| w.set_sync_engine(v));
    push_str(w.get_sync_kopia_password(), &form.kopia_password, |v| {
        w.set_sync_kopia_password(v)
    });
    push_bool(
        w.get_sync_connection_revealed(),
        form.connection_revealed,
        |v| w.set_sync_connection_revealed(v),
    );
    push_bool(w.get_sync_busy(), form.busy, |v| w.set_sync_busy(v));
    push_bool(
        w.get_sync_confirm_master_delete(),
        form.confirm_master_delete,
        |v| w.set_sync_confirm_master_delete(v),
    );

    let message = form.msg.clone().unwrap_or_default();
    let ok = !message.contains("失败") && !message.contains("不一样") && !message.contains("请先");
    push_str(w.get_sync_message(), &message, |v| w.set_sync_message(v));
    push_bool(w.get_sync_message_ok(), ok, |v| w.set_sync_message_ok(v));

    // ── 只读的一半 ──
    push_bool(w.get_sync_loaded(), form.loaded, |v| w.set_sync_loaded(v));
    let (has_key_id, has_app_key) = match status {
        Some(status) => (
            status.has_secret("b2-key-id"),
            status.has_secret("b2-app-key"),
        ),
        None => (false, false),
    };
    let empty = SyncStatus::default();
    let status = status.unwrap_or(&empty);
    push_str(w.get_sync_remote(), &status.remote, |v| {
        w.set_sync_remote(v)
    });
    push_str(
        w.get_sync_rclone(),
        status.rclone.as_deref().unwrap_or_default(),
        |v| w.set_sync_rclone(v),
    );
    push_str(
        w.get_sync_kopia_binary(),
        status.kopia.as_deref().unwrap_or_default(),
        |v| w.set_sync_kopia_binary(v),
    );
    // 引擎只推一次(上面表单那一处):它既是可编辑项、又是状态显示项,
    // 推两次会让用户刚点的选择被随后的 status 覆盖。
    push_str(w.get_sync_kopia_prefix(), &status.kopia_prefix, |v| {
        w.set_sync_kopia_prefix(v)
    });
    // 密码状态只说"是不是默认",绝不说值 —— 值从来没离开过凭据库。
    let kopia_password_set = status.has_secret("kopia-password");
    push_bool(w.get_sync_kopia_using_default(), !kopia_password_set, |v| {
        w.set_sync_kopia_using_default(v)
    });
    push_str(
        w.get_sync_kopia_password_state(),
        if kopia_password_set {
            "已自己设置(存在凭据库里)"
        } else {
            "默认的 kotori"
        },
        |v| w.set_sync_kopia_password_state(v),
    );
    push_str(w.get_sync_keyring(), &status.keyring, |v| {
        w.set_sync_keyring(v)
    });
    push_bool(w.get_sync_ephemeral(), status.ephemeral, |v| {
        w.set_sync_ephemeral(v)
    });
    push_str(
        w.get_sync_keyring_hint(),
        crate::secrets::keyring_hint(),
        |v| w.set_sync_keyring_hint(v),
    );
    push_str(
        w.get_sync_problem(),
        status.problem.as_deref().unwrap_or_default(),
        |v| w.set_sync_problem(v),
    );
    push_bool(w.get_sync_ready(), status.ready, |v| w.set_sync_ready(v));
    push_int(w.get_sync_store_kind(), status.store().index(), |v| {
        w.set_sync_store_kind(v)
    });
    // 说"存到哪"/"存在哪"都用这一级自己的名字:没有密钥环的机器上凭据只在内存里,
    // 文案写成"密钥环"就是在骗用户。
    push_str(w.get_sync_store_name(), status.store().name(), |v| {
        w.set_sync_store_name(v)
    });
    push_str(w.get_sync_store_path(), &status.store_path, |v| {
        w.set_sync_store_path(v)
    });
    push_str(w.get_sync_master_file(), &status.master_file, |v| {
        w.set_sync_master_file(v)
    });
    push_bool(w.get_sync_store_locked(), status.store_locked, |v| {
        w.set_sync_store_locked(v)
    });
    push_bool(
        w.get_sync_has_credentials(),
        has_key_id || has_app_key,
        |v| w.set_sync_has_credentials(v),
    );
    push_str(
        w.get_sync_credentials_label(),
        &credentials_label(has_key_id, has_app_key, status.store().name()),
        |v| w.set_sync_credentials_label(v),
    );
    push_str(
        w.get_sync_master_hint(),
        &format!("至少 {} 位,自己记得住就行", app.min_master_password()),
        |v| w.set_sync_master_hint(v),
    );
}
/// 配对表：一句话（扫描结果）+ 一张表。
///
/// 表本身由 [`Ui::pairing`] 持有（Slint 的数组属性不可变），这里只把模型推过去。
pub(super) fn push_pairing(ui: &mut Ui) {
    let app = &ui.app;
    // 状态在一个 Slint 全局里（见 `widgets/pairing.slint`）：页面只管画，不转发。
    let board = ui.window.global::<PairingBoard>();

    push_bool(board.get_scanned(), app.pairing_scanned, |v| {
        board.set_scanned(v)
    });
    push_bool(board.get_scanning(), app.scanning, |v| {
        board.set_scanning(v)
    });
    push_str(
        board.get_message(),
        app.pairing_msg.as_deref().unwrap_or(""),
        |v| board.set_message(v),
    );
    push_bool(board.get_ok(), app.pairing_ok, |v| board.set_ok(v));

    let items: Vec<PairingItem> = app
        .pairing
        .iter()
        .map(|row| PairingItem {
            cloud_key: row.cloud_key.clone().into(),
            cloud_id: row.cloud_id.clone().into(),
            cloud_short: crate::sync::cloud::short_id(&row.cloud_id, 8).into(),
            cloud_name: row.cloud_name.clone().into(),
            machines: row.machines as i32,
            state: row.state as i32,
            local_id: row.local_id.clone().into(),
            local_name: row.local_name.clone().into(),
            detail: row.detail().into(),
            choices: ModelRc::new(VecModel::from(
                row.choices
                    .iter()
                    .map(|(id, name)| PairingChoice {
                        local_id: id.clone().into(),
                        local_name: name.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    push_model(&ui.pairing, items);
}

/// `sync.status` reports the credential store by name; the page wants a number
/// (it decides which of the three blocks to draw). The mapping itself lives on
/// [`CredentialStore`] — the wording and the index must not drift apart.
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

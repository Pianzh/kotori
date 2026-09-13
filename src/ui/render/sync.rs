//! 「云同步」页:连接与保留、凭据、同步密码。

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
    push_bool(w.get_sync_encryption(), form.encryption, |v| {
        w.set_sync_encryption(v)
    });
    push_int(
        w.get_sync_confirm_encryption(),
        match form.confirm_encryption {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        |v| w.set_sync_confirm_encryption(v),
    );
    push_str(w.get_sync_key_id(), &form.key_id, |v| w.set_sync_key_id(v));
    push_str(w.get_sync_app_key(), &form.app_key, |v| {
        w.set_sync_app_key(v)
    });
    push_str(w.get_sync_password(), &form.password, |v| {
        w.set_sync_password(v)
    });
    push_str(w.get_sync_password_again(), &form.password_again, |v| {
        w.set_sync_password_again(v)
    });
    push_str(w.get_sync_master_password(), &form.master_password, |v| {
        w.set_sync_master_password(v)
    });
    push_bool(w.get_sync_busy(), form.busy, |v| w.set_sync_busy(v));

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
    push_int(
        w.get_sync_store_kind(),
        store_kind_index(&status.store_kind),
        |v| w.set_sync_store_kind(v),
    );
    push_bool(w.get_sync_store_locked(), status.store_locked, |v| {
        w.set_sync_store_locked(v)
    });
    push_str(w.get_sync_store_path(), &status.store_path, |v| {
        w.set_sync_store_path(v)
    });
    push_bool(
        w.get_sync_has_password(),
        status.has_secret("sync-password"),
        |v| w.set_sync_has_password(v),
    );
    push_bool(
        w.get_sync_has_credentials(),
        has_key_id || has_app_key,
        |v| w.set_sync_has_credentials(v),
    );
    push_str(
        w.get_sync_credentials_label(),
        &credentials_label(has_key_id, has_app_key),
        |v| w.set_sync_credentials_label(v),
    );
    push_str(w.get_sync_password_hint(), &status.password_hint, |v| {
        w.set_sync_password_hint(v)
    });
    push_str(
        w.get_sync_master_hint(),
        &format!("至少 {} 位,自己记得住就行", app.min_master_password()),
        |v| w.set_sync_master_hint(v),
    );
}
/// `sync.status` reports the credential store by name; the page wants a number
/// (it decides which of the three blocks to draw).
fn store_kind_index(kind: &str) -> i32 {
    match kind {
        "encrypted-file" => 1,
        "session-only" => 2,
        _ => 0,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_credential_store_names_the_three_places_a_secret_can_live() {
        assert_eq!(store_kind_index("system"), 0);
        assert_eq!(store_kind_index("encrypted-file"), 1);
        assert_eq!(store_kind_index("session-only"), 2);
        // 不认识的答复按最坏情况算:当作系统密钥环,不吓唬用户。
        assert_eq!(store_kind_index(""), 0);
    }
}

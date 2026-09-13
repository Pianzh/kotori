//! Callbacks: what the window says, turned into a [`Message`].
//!
//! Every handler here does the same two things — build a message, hand it to the
//! loop — with one exception: the per-game page owns an editable copy of the
//! stored profile keyed on `detail-seed`, so opening a game (and pressing
//! "重置") has to bump that seed *after* the loop has pushed the fresh values
//! into the window.
//!
//! Nothing in here decides anything. If a rule appears in this file, it belongs
//! in `update.rs` instead.

use super::*;

/// Index → the enum, for the one callback that carries a page number.
fn tab_at(index: i32) -> Tab {
    match index {
        1 => Tab::Add,
        2 => Tab::Sync,
        3 => Tab::Settings,
        _ => Tab::Games,
    }
}

/// The three save-location kinds, by the index the drop-down reports.
fn save_kind_at(index: i32) -> String {
    SAVE_PATH_KINDS
        .get(index.max(0) as usize)
        .copied()
        .unwrap_or(SAVE_PATH_KINDS[0])
        .to_string()
}

pub(super) fn install_callbacks(window: &AppWindow) {
    // ── 外壳 ──────────────────────────────────────────────────────────────
    window.on_tab_changed(|index| dispatch(Message::TabChanged(tab_at(index))));
    window.on_refresh(|| dispatch(Message::Refresh));

    // ── 游戏库 ────────────────────────────────────────────────────────────
    window.on_search_changed(|text| dispatch(Message::SearchChanged(text.to_string())));
    window.on_open_game(|id| {
        dispatch(Message::GameSelected(id.to_string()));
        with_ui(|ui| {
            ui.window.set_game_open(true);
            // 进页面时抄一份已存值(页面自己拿着可编辑副本,见 game-settings.slint)。
            ui.reseed_detail();
        });
    });
    window.on_launch(|id| dispatch(Message::Launch(id.to_string())));
    window.on_stop(|id| dispatch(Message::Stop(id.to_string())));

    // ── 添加游戏 ──────────────────────────────────────────────────────────
    window.on_new_name_changed(|text| dispatch(Message::NewNameChanged(text.to_string())));
    window.on_new_game_dir_changed(|text| dispatch(Message::NewGameDirChanged(text.to_string())));
    window.on_new_exe_changed(|text| dispatch(Message::NewExeChanged(text.to_string())));
    window.on_create_requested(|| dispatch(Message::CreateRequested));

    // ── 单个游戏 ──────────────────────────────────────────────────────────
    window.on_back(|| {
        dispatch(Message::BackToList);
        with_ui(|ui| ui.window.set_game_open(false));
    });
    window.on_save(|| dispatch(Message::SaveProfile));
    window.on_reset(|| {
        // 把草稿重新按「已存值」铺一遍,再让页面把副本重抄一次 —— 缺了后一半,
        // 用户看到的还是自己改过的内容,而草稿已经回到原样,两边就对不上了。
        let id = with_ui(|ui| ui.window.get_game().id.to_string());
        dispatch(Message::GameSelected(id));
        with_ui(Ui::reseed_detail);
    });
    window.on_delete_requested(|| dispatch(Message::DeleteRequested));
    window.on_delete_cancelled(|| dispatch(Message::DeleteCancelled));
    window.on_delete_confirmed(|| dispatch(Message::DeleteConfirmed));

    window.on_game_dir_changed(|text| dispatch(Message::GameDirChanged(text.to_string())));
    window.on_exe_changed(|text| dispatch(Message::ExePathChanged(text.to_string())));
    window.on_ratio_changed(|text| dispatch(Message::ScaleRatioChanged(text.to_string())));
    window.on_algo_picked(|index| {
        if let Some(label) = ScaleAlgorithm::ALL.get(index.max(0) as usize) {
            dispatch(Message::AlgoChanged((*label).to_string()));
        }
    });
    window.on_sharpness_changed(|value| dispatch(Message::SharpnessChanged(value as f32)));
    window.on_internal_w_changed(|text| dispatch(Message::InternalWChanged(text.to_string())));
    window.on_internal_h_changed(|text| dispatch(Message::InternalHChanged(text.to_string())));
    window.on_output_w_changed(|text| dispatch(Message::OutputWChanged(text.to_string())));
    window.on_output_h_changed(|text| dispatch(Message::OutputHChanged(text.to_string())));
    window.on_fullscreen_toggled(|value| dispatch(Message::FullscreenToggled(value)));
    window.on_framerate_changed(|text| dispatch(Message::FramerateChanged(text.to_string())));
    window.on_add_save(|| dispatch(Message::AddSavePath));
    window.on_remove_save(|index| dispatch(Message::RemoveSavePath(index.max(0) as usize)));
    window.on_save_kind_picked(|index, kind| {
        dispatch(Message::SavePathKindChanged(
            index.max(0) as usize,
            save_kind_at(kind),
        ));
    });
    window.on_save_path_changed(|index, text| {
        dispatch(Message::SavePathChanged(
            index.max(0) as usize,
            text.to_string(),
        ));
    });
    window.on_save_exclude_changed(|index, text| {
        dispatch(Message::SavePathExcludeChanged(
            index.max(0) as usize,
            text.to_string(),
        ));
    });

    // 单游戏设置页的云存档(动作落在同一个游戏上,所以 id 从窗口取)。
    window.on_detail_sync_now(|| {
        let id = with_ui(|ui| ui.window.get_game().id.to_string());
        if !id.is_empty() {
            dispatch(Message::SyncNow(Some(id)));
        }
    });
    window.on_detail_sync_restore(|| {
        let id = with_ui(|ui| ui.window.get_game().id.to_string());
        if !id.is_empty() {
            dispatch(Message::SyncRestoreRequested(id, None));
        }
    });
    window.on_detail_sync_restore_confirmed(|| dispatch(Message::SyncRestoreConfirmed));
    window.on_detail_sync_restore_cancelled(|| dispatch(Message::SyncRestoreCancelled));

    // ── 云同步 ────────────────────────────────────────────────────────────
    window.on_sync_enabled_toggled(|value| dispatch(Message::SyncToggleEnabled(value)));
    window.on_sync_encryption_toggled(|value| dispatch(Message::SyncEncryptionToggled(value)));
    window.on_sync_confirm_encryption_clicked(|| dispatch(Message::SyncConfirmEncryption));
    window.on_sync_cancel_encryption_clicked(|| dispatch(Message::SyncCancelEncryption));
    window.on_sync_field(|field, text| {
        let text = text.to_string();
        // 8 是主密码:它不是 `[sync]` 里的设置项,所以不走 SyncField。
        match field {
            0 => dispatch(Message::SyncField(SyncField::Endpoint, text)),
            1 => dispatch(Message::SyncField(SyncField::Bucket, text)),
            2 => dispatch(Message::SyncField(SyncField::Prefix, text)),
            3 => dispatch(Message::SyncField(SyncField::KeepVersions, text)),
            4 => dispatch(Message::SyncField(SyncField::KeyId, text)),
            5 => dispatch(Message::SyncField(SyncField::AppKey, text)),
            6 => dispatch(Message::SyncField(SyncField::Password, text)),
            7 => dispatch(Message::SyncField(SyncField::PasswordAgain, text)),
            _ => dispatch(Message::SyncMasterPasswordChanged(text)),
        }
    });
    window.on_sync_save_settings(|| dispatch(Message::SyncSaveSettings));
    window.on_sync_test(|| dispatch(Message::SyncTest));
    window.on_sync_now(|id| {
        let id = id.to_string();
        dispatch(Message::SyncNow((!id.is_empty()).then_some(id)));
    });
    window.on_sync_restore(|id| {
        dispatch(Message::SyncRestoreRequested(id.to_string(), None));
    });
    window.on_sync_restore_confirmed(|| dispatch(Message::SyncRestoreConfirmed));
    window.on_sync_restore_cancelled(|| dispatch(Message::SyncRestoreCancelled));
    window.on_sync_save_credentials(|| dispatch(Message::SyncSaveCredentials));
    window.on_sync_clear_credentials(|| dispatch(Message::SyncClearCredentials));
    window.on_sync_save_password(|| dispatch(Message::SyncSavePassword));
    window.on_sync_clear_password(|| dispatch(Message::SyncClearPassword));
    window.on_sync_unlock(|| dispatch(Message::SyncUnlock));
    window.on_sync_set_master_password(|| dispatch(Message::SyncSetMasterPassword));
    window.on_sync_lock_credentials(|| dispatch(Message::SyncLockCredentials));
    window.on_sync_delete_master_requested(|| dispatch(Message::SyncMasterDeleteRequested));
    window.on_sync_delete_master_cancelled(|| dispatch(Message::SyncMasterDeleteCancelled));
    window.on_sync_delete_master_confirmed(|| dispatch(Message::SyncMasterDeleteConfirmed));

    // ── 设置 ──────────────────────────────────────────────────────────────
    window.on_wine_prefix_changed(|text| dispatch(Message::WinePrefixChanged(text.to_string())));
    window.on_save_wine_prefix(|| dispatch(Message::SaveWinePrefix));
    window.on_clear_wine_prefix(|| dispatch(Message::ClearWinePrefix));
}

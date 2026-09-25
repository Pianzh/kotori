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

/// 单行输入框里的文本清洗 —— 与 `controls.slint` 里那段"粘贴带换行会让文字向下偏移"
/// 的注释成对。
///
/// 为什么非在这里做不可:Windows 剪贴板的行尾是 `\r\n`,而 Slint 的 `TextInput` 在
/// `single-line` 下**只把 `\n` 换成空格、把 `\r` 原样留下**(`items/text.rs` 的
/// `insert_text`),布局引擎又把孤立的 `\r` 当成强制换行 ⇒ 框里多出一个空行,经
/// `vertical-alignment: center` 居中后看着就是"文字向下偏移"(用户 2026-09-18 报的,
/// 只在 Windows 出现:Linux 剪贴板是 `\n`,会被换成空格)。`.slint` 没有字符串处理
/// 原语,所以清洗只能落在 Rust 这一侧。
///
/// 清洗后的值经 [`crate::ui::render`] 的"先比再写"回灌进控件 —— 那边只在值**不一样**
/// 时才写,所以这一次回写是有效的(控件里躺着的是带 `\r` 的原串),而正常的逐字输入
/// 不会被它打断。
fn one_line(text: &str) -> String {
    // `\r\n` 是**一个**换行,别换成两个空格(参数之间多一个空格是良性的,但没必要)。
    // 换成空格而不是删掉,与 TextInput 自己处理 `\n` 的方式一致:`-f\n-W 1920` 那种
    // 粘贴不会被粘成一个词。
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// 密钥类字段(applicationKey、主密码、仓库密码)专用:换行**直接删掉**。
///
/// 空格在这里不是"无害的留白",它是密钥的一部分 —— 粘一个行尾进来就等于把密钥改坏了,
/// 而报错会晚到"连不上"那一步。
fn no_breaks(text: &str) -> String {
    text.replace(['\r', '\n'], "")
}

/// Index → the enum, for the one callback that carries a page number.
fn tab_at(index: i32) -> Tab {
    match index {
        1 => Tab::Add,
        2 => Tab::Cloud,
        3 => Tab::Sync,
        4 => Tab::Settings,
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
    window.on_search_changed(|text| dispatch(Message::SearchChanged(one_line(&text))));
    window.on_open_game(|id| {
        dispatch(Message::GameSelected(id.to_string()));
        with_ui(|ui| {
            ui.window.set_game_open(true);
            // 进页面时抄一份已存值(页面自己拿着可编辑副本,见 game-settings.slint)。
            ui.reseed_detail();
        });
    });
    // 一颗按钮两种时候 —— 该启动还是该停由 Rust 决定,界面不自己分岔。
    window.on_toggle_run(|id| dispatch(Message::ToggleRun(id.to_string())));

    // ── 添加游戏 ──────────────────────────────────────────────────────────
    window.on_new_name_changed(|text| dispatch(Message::NewNameChanged(one_line(&text))));
    window.on_new_game_dir_changed(|text| dispatch(Message::NewGameDirChanged(one_line(&text))));
    window.on_new_exe_changed(|text| dispatch(Message::NewExeChanged(one_line(&text))));
    window.on_new_game_dir_disk_changed(|text| {
        dispatch(Message::NewGameDirDiskChanged(one_line(&text)))
    });
    window.on_new_game_dir_relative_changed(|text| {
        dispatch(Message::NewGameDirRelativeChanged(one_line(&text)))
    });
    window.on_new_exe_disk_changed(|text| dispatch(Message::NewExeDiskChanged(one_line(&text))));
    window.on_new_exe_relative_changed(|text| {
        dispatch(Message::NewExeRelativeChanged(one_line(&text)))
    });
    window.on_create_requested(|| dispatch(Message::CreateRequested));
    window.on_browse_new_game_dir(|| dispatch(Message::PickPath(PathTarget::NewGameDir)));
    window.on_browse_new_exe(|| dispatch(Message::PickPath(PathTarget::NewExe)));

    // ── 单个游戏 ──────────────────────────────────────────────────────────
    window.on_back(|| {
        dispatch(Message::BackToList);
        with_ui(|ui| ui.window.set_game_open(false));
    });
    window.on_reset(|| {
        // 把草稿重新按「已存值」铺一遍,再让页面把副本重抄一次 —— 缺了后一半,
        // 用户看到的还是自己改过的内容,而草稿已经回到原样,两边就对不上了。
        dispatch(Message::ResetProfile);
        with_ui(Ui::reseed_detail);
    });
    window.on_delete_requested(|| dispatch(Message::DeleteRequested));
    window.on_delete_cancelled(|| dispatch(Message::DeleteCancelled));
    window.on_delete_confirmed(|| dispatch(Message::DeleteConfirmed));

    window.on_game_dir_changed(|text| dispatch(Message::GameDirChanged(one_line(&text))));
    window.on_exe_changed(|text| dispatch(Message::ExePathChanged(one_line(&text))));
    window.on_game_dir_disk_changed(|text| dispatch(Message::GameDirDiskChanged(one_line(&text))));
    window.on_game_dir_relative_changed(|text| {
        dispatch(Message::GameDirRelativeChanged(one_line(&text)))
    });
    window.on_exe_disk_changed(|text| dispatch(Message::ExeDiskChanged(one_line(&text))));
    window.on_exe_relative_changed(|text| dispatch(Message::ExeRelativeChanged(one_line(&text))));
    // 「路径」与「存档位置」两组各自的保存按钮（这两组不自动保存）。
    window.on_save_paths(|| dispatch(Message::SaveGroup(SaveScope::Paths)));
    window.on_save_saves(|| dispatch(Message::SaveGroup(SaveScope::Saves)));
    window.on_launch_args_changed(|text| dispatch(Message::LaunchArgsChanged(one_line(&text))));
    window
        .on_gamescope_args_changed(|text| dispatch(Message::GamescopeArgsChanged(one_line(&text))));
    window.on_browse_game_dir(|| dispatch(Message::PickPath(PathTarget::GameDir)));
    window.on_browse_exe(|| dispatch(Message::PickPath(PathTarget::Exe)));
    window.on_browse_save(|index| {
        dispatch(Message::PickPath(PathTarget::SavePath(
            index.max(0) as usize
        )))
    });
    window.on_ratio_changed(|text| dispatch(Message::ScaleRatioChanged(one_line(&text))));
    window.on_algo_picked(|index| {
        if let Some(label) = ScaleAlgorithm::ALL.get(index.max(0) as usize) {
            dispatch(Message::AlgoChanged((*label).to_string()));
        }
    });
    window.on_sharpness_changed(|value| dispatch(Message::SharpnessChanged(value as f32)));
    window.on_internal_w_changed(|text| dispatch(Message::InternalWChanged(one_line(&text))));
    window.on_internal_h_changed(|text| dispatch(Message::InternalHChanged(one_line(&text))));
    window.on_output_w_changed(|text| dispatch(Message::OutputWChanged(one_line(&text))));
    window.on_output_h_changed(|text| dispatch(Message::OutputHChanged(one_line(&text))));
    window.on_direct_launch_toggled(|value| dispatch(Message::DirectLaunchToggled(value)));
    window.on_auto_watch_toggled(|value| dispatch(Message::AutoWatchToggled(value)));
    // 单游戏页那块「云存档」：状态在一个全局里（见 `pages/game-sync.slint`），回调也从
    // 那儿接（照 `CloudBoard`）。
    let game_sync = window.global::<GameSyncBoard>();
    game_sync.on_toggled(|value| dispatch(Message::SyncParticipatingToggled(value)));
    game_sync.on_rebind_requested(|| dispatch(Message::SyncRebindRequested));
    game_sync.on_new_identity_confirmed(|| dispatch(Message::SyncNewIdentityConfirmed));
    game_sync.on_new_identity_cancelled(|| dispatch(Message::SyncNewIdentityCancelled));
    window
        .on_process_name_changed(|value| dispatch(Message::ProcessNameChanged(value.to_string())));
    window.on_stop_cancelled(|| dispatch(Message::StopCancelled));
    // 「从正在运行的进程里挑」:状态在一个 Slint 全局里,所以它的回调也从那儿接
    // —— 添加游戏页那颗按钮直接喊全局,不必经窗口转发。
    let picker = window.global::<ProcessPickerState>();
    picker.on_pick_for_new_game(|| dispatch(Message::ProcessPickerOpen));
    picker.on_query_changed(|value| dispatch(Message::ProcessQueryChanged(value.to_string())));
    picker.on_picked(|index| dispatch(Message::ProcessPicked(index as usize)));
    picker.on_closed(|| dispatch(Message::ProcessPickerClose));
    window.on_fullscreen_toggled(|value| dispatch(Message::FullscreenToggled(value)));
    window.on_framerate_changed(|text| dispatch(Message::FramerateChanged(one_line(&text))));
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
            one_line(&text),
        ));
    });
    window.on_save_exclude_changed(|index, text| {
        dispatch(Message::SavePathExcludeChanged(
            index.max(0) as usize,
            one_line(&text),
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
    let sync_form = window.global::<SyncBoard>();
    sync_form.on_enabled_toggled(|value| dispatch(Message::SyncToggleEnabled(value)));
    sync_form.on_engine_selected(|engine| {
        dispatch(Message::SyncEngineSelected(engine.to_string()));
    });
    sync_form.on_field(|field, text| {
        // 5 是 applicationKey、6 是主密码:那两个里空格会混进密钥,换行必须**删掉**;
        // 其余都是普通单行文本,换行换成空格即可(见 `one_line`)。
        let text = if field == 5 || field == 6 {
            no_breaks(&text)
        } else {
            one_line(&text)
        };
        // 6 是主密码、7 是 kopia 仓库密码:两个都不是 `[sync]` 里的设置项,
        // 所以都不走 SyncField(它们进的是凭据库,不是配置文件)。
        match field {
            0 => dispatch(Message::SyncField(SyncField::Endpoint, text)),
            1 => dispatch(Message::SyncField(SyncField::Bucket, text)),
            2 => dispatch(Message::SyncField(SyncField::Prefix, text)),
            3 => dispatch(Message::SyncField(SyncField::KeepVersions, text)),
            4 => dispatch(Message::SyncField(SyncField::KeyId, text)),
            5 => dispatch(Message::SyncField(SyncField::AppKey, text)),
            7 => dispatch(Message::SyncKopiaPasswordChanged(text)),
            // 8/9 是两个引擎的"程序位置"：它们是 `[sync]` 里的设置项，走 SyncField。
            8 => dispatch(Message::SyncField(SyncField::RcloneBinary, text)),
            9 => dispatch(Message::SyncField(SyncField::KopiaBinary, text)),
            _ => dispatch(Message::SyncMasterPasswordChanged(text)),
        }
    });
    sync_form.on_save_kopia_password(|| dispatch(Message::SyncSaveKopiaPassword));
    sync_form.on_save_settings(|| dispatch(Message::SyncSaveSettings));
    // 「程序位置」的两个「浏览…」：借系统对话框挑目录（或可执行文件所在的目录）。
    sync_form.on_browse_rclone_binary(|| dispatch(Message::PickPath(PathTarget::RcloneBinary)));
    sync_form.on_browse_kopia_binary(|| dispatch(Message::PickPath(PathTarget::KopiaBinary)));
    sync_form.on_test(|| dispatch(Message::SyncTest));
    // 启动前那一问：回答与取消都从一个 Slint 全局来（照 `CloudBoard`）。
    let ask = window.global::<SyncAskState>();
    ask.on_answered(|choice| dispatch(Message::SyncAskAnswered(choice.to_string())));
    ask.on_pair_requested(|| dispatch(Message::SyncAskPairRequested));
    ask.on_bind_found(|| dispatch(Message::SyncAskBindFound));
    // 「云端存档」页：刷新读索引、深度扫描读所有卡、点开一款再问一次版本、搜索是本地过滤。
    let cloud = window.global::<CloudBoard>();
    cloud.on_refresh(|| dispatch(Message::CloudRefresh));
    cloud.on_scan(|| dispatch(Message::CloudScan));
    cloud.on_search_changed(|text| dispatch(Message::CloudSearch(one_line(&text))));
    cloud.on_toggle(|key| dispatch(Message::CloudToggle(key.to_string())));
    cloud.on_back(|| dispatch(Message::CloudBack));
    // 单游戏页那一页「这一款的云端存档」：状态在一个全局里（见 `pages/game-versions.slint`）。
    let versions = window.global::<GameVersionsBoard>();
    versions.on_back(|| dispatch(Message::GameVersionsClosed));
    versions.on_replace(|name| dispatch(Message::GameVersionsReplace(name.to_string())));
    versions.on_replace_confirmed(|| dispatch(Message::GameVersionsReplaceConfirmed));
    versions.on_replace_cancelled(|| dispatch(Message::GameVersionsReplaceCancelled));
    // 那一页里的「更改绑定…」与身份条上那颗是同一件事，走同一条消息。
    versions.on_rebind_requested(|| dispatch(Message::SyncRebindRequested));
    // 身份条**整条可点**：进去看这一款在云端存了哪几版。
    let game_sync = window.global::<GameSyncBoard>();
    game_sync.on_cloud_requested(|| dispatch(Message::GameVersionsOpened));
    // 添加页那块云端匹配：挑一条 / 「不是这一款」/ 改主意（照 `CloudBoard`）。
    let add_match = window.global::<AddMatchBoard>();
    add_match.on_choose(|cloud_id| dispatch(Message::MatchChoose(cloud_id.to_string())));
    add_match.on_decline(|| dispatch(Message::MatchDecline));
    add_match.on_restore(|| dispatch(Message::MatchUndoDecline));
    add_match.on_clear_pick(|| dispatch(Message::MatchClearPick));
    // 它旁边那颗「自己选…」：从云端清单里挑一条绑上（状态在一个全局里，回调也从那儿接）。
    let cloud_pick = window.global::<CloudPickerState>();
    cloud_pick.on_open_for_add(|| dispatch(Message::CloudPickOpen(CloudPickPurpose::Add)));
    cloud_pick.on_open_for_launch(|| dispatch(Message::CloudPickOpen(CloudPickPurpose::Launch)));
    cloud_pick.on_open_for_rebind(|| dispatch(Message::CloudPickOpen(CloudPickPurpose::Rebind)));
    cloud_pick.on_new_identity(|| dispatch(Message::CloudPickNewIdentity));
    cloud_pick.on_query_changed(|text| dispatch(Message::CloudPickSearch(one_line(&text))));
    cloud_pick.on_chosen(|cloud_id| dispatch(Message::CloudPickChoose(cloud_id.to_string())));
    cloud_pick.on_closed(|| dispatch(Message::CloudPickDismiss));
    sync_form.on_now(|id| {
        let id = id.to_string();
        dispatch(Message::SyncNow((!id.is_empty()).then_some(id)));
    });
    sync_form.on_restore(|id| {
        dispatch(Message::SyncRestoreRequested(id.to_string(), None));
    });
    sync_form.on_restore_confirmed(|| dispatch(Message::SyncRestoreConfirmed));
    sync_form.on_restore_cancelled(|| dispatch(Message::SyncRestoreCancelled));
    sync_form.on_save_credentials(|| dispatch(Message::SyncSaveCredentials));
    sync_form.on_clear_credentials(|| dispatch(Message::SyncClearCredentials));
    sync_form.on_unlock(|| dispatch(Message::SyncUnlock));
    sync_form.on_set_master_password(|| dispatch(Message::SyncSetMasterPassword));
    sync_form.on_lock_credentials(|| dispatch(Message::SyncLockCredentials));
    sync_form.on_delete_master_requested(|| dispatch(Message::SyncMasterDeleteRequested));
    sync_form.on_delete_master_cancelled(|| dispatch(Message::SyncMasterDeleteCancelled));
    sync_form.on_delete_master_confirmed(|| dispatch(Message::SyncMasterDeleteConfirmed));

    // ── 设置 ──────────────────────────────────────────────────────────────
    window.on_service_start(|| dispatch(Message::ServiceStart));
    window.on_service_stop(|| dispatch(Message::ServiceStop));
    window.on_wine_prefix_changed(|text| dispatch(Message::WinePrefixChanged(one_line(&text))));
    window.on_browse_wine_prefix(|| dispatch(Message::PickPath(PathTarget::WinePrefix)));
    window.on_save_wine_prefix(|| dispatch(Message::SaveWinePrefix));
    window.on_clear_wine_prefix(|| dispatch(Message::ClearWinePrefix));
    // 「重新检查」只是让 daemon 再探一遍;探测本身在 `crate::platform`。
    window.on_env_reload(|| dispatch(Message::EnvironmentReload));
    window.on_config_source_picked(|portable| dispatch(Message::ConfigSourcePicked(portable)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 剪贴板那种行尾(`\r\n`)粘进单行框之后必须不再剩任何换行
    /// —— 孤立的 `\r` 就是"文字向下偏移"的成因。
    #[test]
    fn one_line_leaves_no_line_breaks_behind() {
        for (raw, want) in [
            ("F:\\BTL\\game\\game.exe\r\n", "F:\\BTL\\game\\game.exe "),
            ("-f\r\n-W 1920", "-f -W 1920"),
            ("savedata\r", "savedata "),
            ("普通文本", "普通文本"),
        ] {
            let got = one_line(raw);
            assert_eq!(got, want, "for {raw:?}");
            assert!(!got.contains('\r') && !got.contains('\n'), "{got:?}");
        }
    }

    /// 密钥类字段里空格是密钥的一部分:换行只能删,不能换成空格。
    #[test]
    fn no_breaks_deletes_instead_of_replacing() {
        assert_eq!(no_breaks("005a1b2c\r\n"), "005a1b2c");
        assert_eq!(no_breaks("a b"), "a b", "本来就在里面的空格不许动");
    }
}

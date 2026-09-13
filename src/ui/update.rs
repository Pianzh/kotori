//! `App::update`: the message loop.

use super::*;

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabChanged(tab) => {
                self.tab = tab;
                self.selected = None;
                self.draft = None;
                self.confirm_delete = false;
                self.error = None;
                if tab == Tab::Settings || tab == Tab::Sync {
                    // Re-read them all, they may have changed on disk (or in the
                    // daemon, which is the only writer).
                    return Task::batch([
                        Task::perform(
                            async { load_wine_status().await },
                            Message::WineStatusLoaded,
                        ),
                        Task::perform(
                            async { load_sync_status().await },
                            Message::SyncStatusLoaded,
                        ),
                        Task::perform(async { load_hotkeys().await }, Message::HotkeysLoaded),
                    ]);
                }
                Task::none()
            }
            Message::Refresh => {
                self.error = None;
                self.loading = true;
                Task::perform(async { connect_and_load().await }, Message::GamesLoaded)
            }
            Message::GamesLoaded(Ok(games)) => {
                self.games = games;
                self.loading = false;
                self.daemon_connected = Some(true);
                self.error = None;
                self.retry_attempts = 0;
                Task::none()
            }
            Message::GamesLoaded(Err(e)) => {
                self.loading = false;
                self.daemon_connected = Some(false);
                self.error = Some(e);
                // Self-heal: keep retrying with backoff, so the UI recovers on
                // its own once the daemon is back.
                self.retry_attempts = self.retry_attempts.saturating_add(1);
                if self.retry_attempts <= MAX_AUTO_RETRIES {
                    let delay = retry_delay(self.retry_attempts);
                    return Task::perform(async move { tokio::time::sleep(delay).await }, |_| {
                        Message::Refresh
                    });
                }
                Task::none()
            }
            Message::Launch(id) => {
                self.launching = Some(id.clone());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move {
                        let mut params = serde_json::Map::new();
                        params.insert("id".into(), Value::String(id));
                        crate::rpc::call(&socket, "game.launch", Some(params)).await
                    },
                    Message::LaunchDone,
                )
            }
            Message::LaunchDone(result) => {
                self.launching = None;
                match result {
                    Ok(value) => {
                        let sid = value
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        tracing::info!("game session started: {sid}");
                        self.error = None;
                        let socket = self.daemon_socket.clone();
                        return Task::perform(
                            async move { load_status(&socket).await },
                            Message::StatusLoaded,
                        );
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::GameSelected(id) => {
                let mut load_sync = false;
                if let Some(g) = self.games.iter().find(|g| g.id == id) {
                    self.selected = Some(g.id.clone());
                    self.saved_msg = None;
                    self.confirm_delete = false;
                    // Seed the form from the *stored* profile. Anything else
                    // means a plain "open + save" silently rewrites settings.
                    self.draft = Some(Draft::from_game(g));
                    // 这一页现在也显示这个游戏的云存档状况,而 `sync.status` 平时只在
                    // 打开「云同步」/「设置」页时才读 —— 直接从游戏库点进来时补一次。
                    load_sync = self.sync_status.is_none();
                }
                if load_sync {
                    return Task::perform(
                        async { load_sync_status().await },
                        Message::SyncStatusLoaded,
                    );
                }
                Task::none()
            }
            Message::BackToList => {
                self.selected = None;
                self.draft = None;
                self.saved_msg = None;
                self.confirm_delete = false;
                Task::none()
            }
            Message::SearchChanged(query) => {
                self.search = query;
                Task::none()
            }
            Message::AlgoChanged(algo) => {
                if let Some(d) = &mut self.draft {
                    d.algo = algo;
                }
                Task::none()
            }
            Message::SharpnessChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.sharpness = v.round() as u32;
                }
                Task::none()
            }
            Message::InternalWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_w = v;
                }
                Task::none()
            }
            Message::InternalHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_h = v;
                }
                Task::none()
            }
            Message::OutputWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_w = v;
                }
                Task::none()
            }
            Message::OutputHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_h = v;
                }
                Task::none()
            }
            Message::ScaleRatioChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.scale_ratio = v;
                }
                Task::none()
            }
            Message::FullscreenToggled(b) => {
                if let Some(d) = &mut self.draft {
                    d.fullscreen = b;
                }
                Task::none()
            }
            Message::FramerateChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.framerate = v;
                }
                Task::none()
            }
            Message::ExePathChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.exe = v;
                }
                Task::none()
            }
            Message::DeleteRequested => {
                self.confirm_delete = true;
                Task::none()
            }
            Message::DeleteCancelled => {
                self.confirm_delete = false;
                Task::none()
            }
            Message::DeleteConfirmed => {
                let Some(game_id) = self.selected.clone() else {
                    return Task::none();
                };
                self.confirm_delete = false;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { remove_game(&socket, &game_id).await },
                    Message::Deleted,
                )
            }
            Message::Deleted(result) => {
                match result {
                    Ok(()) => {
                        self.selected = None;
                        self.draft = None;
                        self.error = None;
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::NewNameChanged(value) => {
                self.new_name = value;
                Task::none()
            }
            Message::NewGameDirChanged(value) => {
                self.new_game_dir = value;
                Task::none()
            }
            Message::NewExeChanged(value) => {
                self.new_exe = value;
                Task::none()
            }
            Message::CreateRequested => {
                let name = self.new_name.trim().to_string();
                let exe = self.new_exe.trim().to_string();
                let game_dir = self.new_game_dir.trim().to_string();
                if name.is_empty() || exe.is_empty() {
                    self.create_msg = Some("游戏名和可执行文件都必须填写".to_string());
                    return Task::none();
                }
                self.creating = true;
                self.create_msg = None;
                self.error = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { create_game(&socket, name, exe, game_dir).await },
                    Message::CreateFinished,
                )
            }
            Message::CreateFinished(result) => {
                self.creating = false;
                match result {
                    Ok(id) => {
                        self.create_msg = Some(format!("已添加（ID: {id}），可在游戏库里继续配置"));
                        self.new_name.clear();
                        self.new_game_dir.clear();
                        self.new_exe.clear();
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::WineStatusLoaded(result) => {
                match result {
                    Ok(status) => {
                        // Do not clobber an edit that is still in progress: this
                        // reply can arrive a second after the user started typing.
                        if !self.wine_prefix_dirty {
                            self.wine_prefix_input = status.configured.clone().unwrap_or_default();
                        }
                        self.wine_status = Some(status);
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::WinePrefixChanged(value) => {
                self.wine_prefix_input = value;
                self.wine_prefix_dirty = true;
                Task::none()
            }
            Message::SaveWinePrefix => {
                let prefix = self.wine_prefix_input.trim().to_string();
                self.wine_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_wine_prefix(&socket, Some(prefix)).await },
                    Message::WinePrefixSaved,
                )
            }
            Message::ClearWinePrefix => {
                self.wine_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_wine_prefix(&socket, None).await },
                    Message::WinePrefixSaved,
                )
            }
            Message::WinePrefixSaved(result) => {
                self.wine_msg = Some(match &result {
                    Ok(()) => "已保存".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon holds it now, so a reload may refill the field.
                    self.wine_prefix_dirty = false;
                } else if let Err(e) = &result {
                    self.error = Some(e.clone());
                }
                Task::perform(
                    async { load_wine_status().await },
                    Message::WineStatusLoaded,
                )
            }
            Message::GameDirChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.game_dir = value;
                }
                Task::none()
            }
            Message::SavePathKindChanged(index, kind) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.kind = kind;
                }
                Task::none()
            }
            Message::SavePathChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.path = value;
                }
                Task::none()
            }
            Message::SavePathExcludeChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.exclude = value;
                }
                Task::none()
            }
            Message::AddSavePath => {
                if let Some(draft) = &mut self.draft {
                    draft.save_paths.push(SavePathDraft {
                        kind: "windows".to_string(),
                        path: "%APPDATA%\\".to_string(),
                        exclude: String::new(),
                    });
                }
                Task::none()
            }
            Message::RemoveSavePath(index) => {
                if let Some(draft) = &mut self.draft
                    && index < draft.save_paths.len()
                {
                    draft.save_paths.remove(index);
                }
                Task::none()
            }
            Message::Tick => {
                let socket = self.daemon_socket.clone();
                let poll = Task::perform(
                    async move { load_status(&socket).await },
                    Message::StatusLoaded,
                );
                let next = Task::perform(async { tokio::time::sleep(STATUS_POLL).await }, |_| {
                    Message::Tick
                });
                Task::batch([poll, next])
            }
            Message::StatusLoaded(Ok(running)) => {
                self.running = running;
                self.daemon_connected = Some(true);
                Task::none()
            }
            Message::StatusLoaded(Err(e)) => {
                // Do not fight the reconnect loop for the error banner; just
                // mark the daemon as gone and let the user see it.
                self.daemon_connected = Some(false);
                tracing::debug!("status poll failed: {e}");
                Task::none()
            }

            Message::HotkeysLoaded(Ok(status)) => {
                self.hotkeys = Some(status);
                Task::none()
            }
            Message::HotkeysLoaded(Err(e)) => {
                // Keep the last answer: "we could not ask" is not "there are no
                // hotkeys", and the page says so on its own when nothing arrived.
                tracing::debug!("hotkey status failed: {e}");
                Task::none()
            }

            Message::SyncStatusLoaded(Ok(status)) => {
                self.sync_form.apply(&status, &status.settings);
                self.sync_status = Some(status);
                Task::none()
            }
            Message::SyncStatusLoaded(Err(e)) => {
                self.sync_form.loaded = true;
                self.sync_form.msg = Some(format!("读取同步状态失败: {e}"));
                Task::none()
            }
            Message::SyncToggleEnabled(value) => {
                self.sync_form.enabled = value;
                self.sync_form.settings_dirty = true;
                Task::none()
            }
            // Flipping encryption decides whether data already in the bucket can
            // be read at all, so it asks once more instead of taking effect.
            Message::SyncEncryptionToggled(value) => {
                if value == self.sync_form.encryption {
                    self.sync_form.confirm_encryption = None;
                } else {
                    self.sync_form.confirm_encryption = Some(value);
                    self.sync_form.msg = Some(if value {
                        "开启加密后，bucket 里已有的明文存档将读不出来（除非换一个 prefix）。再点一次「确认开启」才会生效。".to_string()
                    } else {
                        "关闭加密后，之前加密上传的存档将无法解密。再点一次「确认关闭」才会生效。"
                            .to_string()
                    });
                }
                Task::none()
            }
            Message::SyncConfirmEncryption => {
                if let Some(value) = self.sync_form.confirm_encryption.take() {
                    self.sync_form.encryption = value;
                    self.sync_form.settings_dirty = true;
                    self.sync_form.msg = Some("已勾选，记得点「保存设置」".to_string());
                }
                Task::none()
            }
            Message::SyncCancelEncryption => {
                self.sync_form.confirm_encryption = None;
                self.sync_form.msg = None;
                Task::none()
            }
            Message::SyncField(field, value) => {
                let form = &mut self.sync_form;
                match field {
                    SyncField::Endpoint => form.endpoint = value,
                    SyncField::Bucket => form.bucket = value,
                    SyncField::Prefix => form.prefix = value,
                    SyncField::KeepVersions => form.keep_versions = value,
                    SyncField::KeyId => form.key_id = value,
                    SyncField::AppKey => form.app_key = value,
                    SyncField::Password => form.password = value,
                    SyncField::PasswordAgain => form.password_again = value,
                }
                if matches!(
                    field,
                    SyncField::Endpoint
                        | SyncField::Bucket
                        | SyncField::Prefix
                        | SyncField::KeepVersions
                ) {
                    form.settings_dirty = true;
                }
                Task::none()
            }
            Message::SyncSaveSettings => {
                let force = self.sync_form.confirm_encryption.is_some()
                    || self.sync_form.encryption != self.stored_encryption();
                let form = self.sync_form.clone();
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_settings(&socket, form.patch(force)).await },
                    Message::SyncSettingsSaved,
                )
            }
            Message::SyncSettingsSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    // The daemon refuses an encryption flip that is not confirmed;
                    // show its words rather than a generic failure.
                    Ok(()) => "已保存".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon now holds exactly what the form holds, so a
                    // later status reply may refill the form again.
                    self.sync_form.settings_dirty = false;
                } else {
                    self.sync_form.confirm_encryption = None;
                }
                self.reload_sync()
            }
            Message::SyncSaveCredentials => {
                let key_id = self.sync_form.key_id.trim().to_string();
                let app_key = self.sync_form.app_key.trim().to_string();
                if key_id.is_empty() && app_key.is_empty() {
                    self.sync_form.msg = Some("两个字段都空着：这只会清掉已保存的凭据".to_string());
                    return Task::none();
                }
                // 内存那一级只是"过渡":没有可持久化的后端时,先把主密码设起来,
                // 否则凭据活不过这个守护进程 —— 静默接受等于骗用户(ADR-014)。
                if self.credential_store() == CredentialStore::Session {
                    self.sync_form.msg = Some(CredentialStore::needs_master_password().to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_credentials(&socket, &key_id, &app_key).await },
                    Message::SyncCredentialsSaved,
                )
            }
            Message::SyncCredentialsSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => self.credential_store().saved_note("凭据"),
                    Err(e) => format!("保存凭据失败: {e}"),
                });
                if result.is_ok() {
                    // The daemon consumed them; never echo secrets back.
                    self.sync_form.key_id.clear();
                    self.sync_form.app_key.clear();
                }
                self.reload_sync()
            }
            Message::SyncClearCredentials => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_credentials(&socket, "", "").await },
                    Message::SyncCredentialsCleared,
                )
            }
            Message::SyncCredentialsCleared(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => format!("已从{}里删除 B2 凭据", self.credential_store().name()),
                    Err(e) => format!("删除凭据失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.key_id.clear();
                    self.sync_form.app_key.clear();
                }
                self.reload_sync()
            }
            Message::SyncClearPassword => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_password(&socket, "").await },
                    Message::SyncPasswordCleared,
                )
            }
            Message::SyncPasswordCleared(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) => format!("已从{}里删除同步密码", self.credential_store().name()),
                    Err(e) => format!("删除密码失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.password.clear();
                    self.sync_form.password_again.clear();
                }
                self.reload_sync()
            }
            Message::SyncSavePassword => {
                let password = self.sync_form.password.clone();
                if !password.is_empty() && password != self.sync_form.password_again {
                    self.sync_form.msg = Some("两次输入的密码不一样".to_string());
                    return Task::none();
                }
                // 加密密码尤其不能只留在内存里:重启后连自己上传的存档都解不开。
                // 清空密码走的是 SyncClearPassword,不受这条限制。
                if !password.is_empty() && self.credential_store() == CredentialStore::Session {
                    self.sync_form.msg = Some(CredentialStore::needs_master_password().to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_password(&socket, &password).await },
                    Message::SyncPasswordSaved,
                )
            }
            Message::SyncPasswordSaved(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match &result {
                    Ok(()) if self.sync_form.password.is_empty() => "已清除同步密码".to_string(),
                    Ok(()) => self.credential_store().saved_note("同步密码"),
                    Err(e) => format!("保存密码失败: {e}"),
                });
                if result.is_ok() {
                    self.sync_form.password.clear();
                    self.sync_form.password_again.clear();
                }
                self.reload_sync()
            }
            Message::SyncTest => {
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(async move { sync_test(&socket).await }, Message::SyncTested)
            }
            Message::SyncTested(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(remote) => format!("连接正常：{remote}"),
                    Err(e) => format!("连接失败: {e}"),
                });
                Task::none()
            }
            Message::SyncMasterPasswordChanged(value) => {
                self.sync_form.master_password = value;
                Task::none()
            }
            Message::SyncUnlock => {
                let password = self.sync_form.master_password.clone();
                if password.is_empty() {
                    self.sync_form.msg = Some("请先输入主密码".to_string());
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { unlock_credentials(&socket, &password).await },
                    Message::SyncUnlocked,
                )
            }
            Message::SyncUnlocked(result) => {
                self.sync_form.busy = false;
                match result {
                    Ok(()) => {
                        self.sync_form.master_password.clear();
                        self.sync_form.msg = Some("已解锁".to_string());
                    }
                    Err(e) => self.sync_form.msg = Some(e),
                }
                self.reload_sync()
            }
            Message::SyncSetMasterPassword => {
                let password = self.sync_form.master_password.clone();
                if password.chars().count() < self.min_master_password() {
                    self.sync_form.msg = Some(format!(
                        "主密码至少要 {} 个字符",
                        self.min_master_password()
                    ));
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_master_password(&socket, &password).await },
                    Message::SyncMasterSaved,
                )
            }
            Message::SyncMasterSaved(result) => {
                self.sync_form.busy = false;
                match result {
                    Ok(path) => {
                        self.sync_form.master_password.clear();
                        self.sync_form.msg = Some(format!("凭据已加密保存到 {path}"));
                    }
                    Err(e) => self.sync_form.msg = Some(e),
                }
                self.reload_sync()
            }
            Message::SyncLockCredentials => {
                if self.sync_form.busy {
                    return Task::none();
                }
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { lock_credentials(&socket).await },
                    Message::SyncCredentialsLocked,
                )
            }
            Message::SyncCredentialsLocked(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(()) => "凭据文件已锁定；再要用它得重新输入主密码".to_string(),
                    Err(e) => format!("锁定失败: {e}"),
                });
                self.reload_sync()
            }
            Message::SyncMasterDeleteRequested => {
                self.sync_form.confirm_master_delete = true;
                Task::none()
            }
            Message::SyncMasterDeleteCancelled => {
                self.sync_form.confirm_master_delete = false;
                Task::none()
            }
            Message::SyncMasterDeleteConfirmed => {
                // 破坏性操作:确认过一次就够了,别再让用户点第三下。
                if !self.sync_form.confirm_master_delete {
                    return Task::none();
                }
                self.sync_form.confirm_master_delete = false;
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { clear_master_file(&socket).await },
                    Message::SyncMasterDeleted,
                )
            }
            Message::SyncMasterDeleted(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(()) => "已删除主密码凭据文件（存在里面的凭据一起消失了）".to_string(),
                    Err(e) => format!("删除凭据文件失败: {e}"),
                });
                self.reload_sync()
            }
            Message::SyncNow(game_id) => {
                self.sync_form.busy = true;
                self.sync_form.msg = Some("正在同步…".to_string());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_now(&socket, game_id).await },
                    Message::SyncNowDone,
                )
            }
            Message::SyncNowDone(result) => {
                self.sync_form.busy = false;
                self.sync_form.msg = Some(match result {
                    Ok(summary) => summary,
                    Err(e) => format!("同步失败: {e}"),
                });
                self.reload_sync()
            }
            Message::SyncRestoreRequested(game_id, version) => {
                self.sync_restore_pending = Some((game_id, version));
                Task::none()
            }
            Message::SyncRestoreCancelled => {
                self.sync_restore_pending = None;
                Task::none()
            }
            Message::SyncRestoreConfirmed => {
                let Some((game_id, version)) = self.sync_restore_pending.take() else {
                    return Task::none();
                };
                self.sync_form.busy = true;
                self.sync_form.msg = Some("正在恢复…".to_string());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { sync_restore(&socket, &game_id, version.as_deref()).await },
                    Message::SyncNowDone,
                )
            }
            Message::Stop(game_id) => {
                let Some(session) = self.running.get(&game_id).map(|s| s.session_id.clone()) else {
                    return Task::none();
                };
                let socket = self.daemon_socket.clone();
                self.error = None;
                Task::perform(
                    async move { stop_session(&socket, &session).await },
                    Message::StopDone,
                )
            }
            Message::StopDone(result) => {
                if let Err(e) = result {
                    self.error = Some(e);
                }
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { load_status(&socket).await },
                    Message::StatusLoaded,
                )
            }
            Message::SaveProfile => {
                let Some(draft) = self.draft.clone() else {
                    return Task::none();
                };
                self.saving = true;
                self.saved_msg = None;
                Task::perform(
                    async move { save_profile(draft).await },
                    Message::ProfileSaved,
                )
            }
            Message::ProfileSaved(result) => {
                self.saving = false;
                self.saved_msg = Some(match &result {
                    Ok(()) => "已保存并通知守护进程".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if let Err(e) = &result {
                    self.error = Some(e.clone());
                } else {
                    // Refresh the library so the new scale shows up.
                    return Task::perform(async { connect_and_load().await }, |r| {
                        Message::GamesLoaded(r)
                    });
                }
                Task::none()
            }
        }
    }
}

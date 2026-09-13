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
                // 后台服务是用户自己停的:刷新只看看它在不在,不许顺手把它拉起来。
                if self.daemon_paused {
                    Task::perform(async { load_without_booting().await }, Message::GamesLoaded)
                } else {
                    Task::perform(async { connect_and_load().await }, Message::GamesLoaded)
                }
            }
            Message::GamesLoaded(Ok(games)) => {
                self.games = games;
                self.loading = false;
                self.daemon_connected = Some(true);
                // 连上了就是连上了 —— 无论它是我们拉起来的还是用户从别处起的。
                self.daemon_paused = false;
                self.error = None;
                self.retry_attempts = 0;
                Task::none()
            }
            Message::GamesLoaded(Err(e)) => {
                self.loading = false;
                self.daemon_connected = Some(false);
                self.error = Some(e);
                // 用户亲手停掉的服务不该被退避重试一次次拉起来(那才叫"停不掉")。
                if self.daemon_paused {
                    self.retry_attempts = 0;
                    return Task::none();
                }
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
                self.schedule_auto_save()
            }
            Message::SharpnessChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.sharpness = v.round() as u32;
                }
                self.schedule_auto_save()
            }
            Message::InternalWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_w = v;
                }
                self.schedule_auto_save()
            }
            Message::InternalHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_h = v;
                }
                self.schedule_auto_save()
            }
            Message::OutputWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_w = v;
                }
                self.schedule_auto_save()
            }
            Message::OutputHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_h = v;
                }
                self.schedule_auto_save()
            }
            Message::ScaleRatioChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.scale_ratio = v;
                }
                self.schedule_auto_save()
            }
            Message::FullscreenToggled(b) => {
                if let Some(d) = &mut self.draft {
                    d.fullscreen = b;
                }
                self.schedule_auto_save()
            }
            Message::FramerateChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.framerate = v;
                }
                self.schedule_auto_save()
            }
            Message::ExePathChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.exe = v;
                }
                self.schedule_auto_save()
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
            // ── 后台服务(守护进程)的启停 ────────────────────────────────────
            Message::ServiceStart => {
                self.service_busy = true;
                self.service_msg = None;
                Task::perform(async { start_daemon().await }, Message::ServiceStarted)
            }
            Message::ServiceStarted(result) => {
                self.service_busy = false;
                match result {
                    Ok(message) => {
                        self.daemon_paused = false;
                        self.service_msg = Some(message);
                        // 起来了就把库重新读一遍(顺便把连接状态摆正)。
                        self.retry_attempts = 0;
                        Task::perform(async { connect_and_load().await }, Message::GamesLoaded)
                    }
                    Err(e) => {
                        self.service_msg = Some(format!("启动失败: {e}"));
                        Task::none()
                    }
                }
            }
            Message::ServiceStop => {
                // 先立旗再发请求:回包还没回来时那次会话轮询就可能已经在路上了。
                self.daemon_paused = true;
                self.service_busy = true;
                self.service_msg = None;
                Task::perform(async { stop_daemon().await }, Message::ServiceStopped)
            }
            Message::ServiceStopped(result) => {
                self.service_busy = false;
                match result {
                    Ok(message) => {
                        self.daemon_connected = Some(false);
                        // 会话列表随之作废:守护进程走了,这里再也问不到谁在跑。
                        self.running.clear();
                        self.service_msg = Some(format!(
                            "{message}（正在玩的游戏不受影响,但它退出后不会再自动上传存档）"
                        ));
                    }
                    Err(e) => {
                        // 没停掉就别立那块牌子,否则界面在说一件没发生的事。
                        self.daemon_paused = false;
                        self.service_msg = Some(format!("停止失败: {e}"));
                    }
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
                self.schedule_auto_save()
            }
            Message::SavePathKindChanged(index, kind) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.kind = kind;
                }
                self.schedule_auto_save()
            }
            Message::SavePathChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.path = value;
                }
                self.schedule_auto_save()
            }
            Message::SavePathExcludeChanged(index, value) => {
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|d| d.save_paths.get_mut(index))
                {
                    entry.exclude = value;
                }
                self.schedule_auto_save()
            }
            Message::AddSavePath => {
                if let Some(draft) = &mut self.draft {
                    draft.save_paths.push(SavePathDraft {
                        kind: "windows".to_string(),
                        path: "%APPDATA%\\".to_string(),
                        exclude: String::new(),
                    });
                }
                self.schedule_auto_save()
            }
            Message::RemoveSavePath(index) => {
                if let Some(draft) = &mut self.draft
                    && index < draft.save_paths.len()
                {
                    draft.save_paths.remove(index);
                }
                self.schedule_auto_save()
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
                // 会话轮询有回应就说明它活着 —— 哪怕是别处重新起的。这时"已停止"
                // 那块牌子必须摘掉,不然侧栏说"已连接"、设置页说过"已停止"。
                self.daemon_paused = false;
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
            Message::AutoSave(generation) => {
                // 世代对不上 = 这 700ms 里又改过,这一次作废(防抖就是靠它)。
                if generation != self.autosave_generation {
                    return Task::none();
                }
                self.begin_auto_save()
            }
            Message::ProfileSaved(generation, result) => {
                let Some(attempt) = self.save_in_flight.take() else {
                    // 一笔只回一次,理论上到不了这儿;真到了也别让界面永远停在"保存中"。
                    self.saving = false;
                    return Task::none();
                };
                self.saving = false;
                // 用户可能已经翻到别的游戏去了:回包只能落在它自己那一份草稿上,
                // 否则会把别人的 `*_original` 写成这个游戏的值。
                let same_game = self.selected.as_deref() == Some(attempt.draft.game_id.as_str());

                match &result {
                    Ok(()) => {
                        if same_game {
                            if let Some(draft) = self.draft.as_mut() {
                                // 服务端现在有的就是这一笔带过去的东西 ⇒ 把这些书签
                                // 推进过去,下一次只发改过的字段(游戏盘没挂载时也
                                // 不会因为重发旧路径而白报错)。
                                draft.game_dir_original = attempt.draft.game_dir.clone();
                                draft.exe_original = attempt.draft.exe.clone();
                                draft.save_paths_original = attempt.draft.save_paths.clone();
                            }
                            self.report_saved("已自动保存", true);
                        }
                    }
                    Err(e) => {
                        // 草稿一个字都不动:用户正在打的那半截不能被回包吃掉。
                        if same_game {
                            self.report_saved(format!("保存失败: {e}"), false);
                        } else {
                            // 已经离开那一页了,别把失败吞掉 —— 挂到顶部的错误条上。
                            self.error = Some(format!("自动保存失败: {e}"));
                        }
                    }
                }

                // 这一笔已经过期(按过「重置」,或者又改过):配置里现在写着的是一个
                // 用户不要的值,用手上的草稿再存一次把它拉回来。
                if same_game && generation != self.autosave_generation {
                    return self.begin_auto_save();
                }
                // 存完把库读一遍:列表与"已存值"要跟上,否则退出这一页再进来看到的是旧的。
                // 页面自己的副本不会被它重置(种子没动,见 game-settings.slint)。
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { load_games_from(&socket).await },
                    Message::GamesLoaded,
                )
            }
            Message::ResetProfile => {
                // 作废还挂在防抖窗口里的那一笔,然后把页面重新按「已存值」铺一遍
                // (真正把副本抄回去的是 wire 里的 `reseed_detail`)。
                self.cancel_auto_save();
                let Some(game) = self.selected_game().cloned() else {
                    return Task::none();
                };
                let unchanged = self
                    .draft
                    .as_ref()
                    .is_none_or(|draft| draft.matches_stored(&game));
                self.draft = Some(Draft::from_game(&game));
                self.report_saved(
                    if unchanged {
                        "没有未保存的改动"
                    } else {
                        "已还原为已保存的设置"
                    },
                    true,
                );
                Task::none()
            }
            Message::PickerProbed(result) => {
                if let Err(reason) = &result {
                    // 不是"出错了",是这台机器上确实没有 —— 记一条,界面据此灰掉按钮。
                    tracing::info!("系统文件选择框不可用：{reason}");
                }
                self.picker = Some(result);
                Task::none()
            }
            Message::PickPath(target) => {
                // 框已经开着:再来一次只会弹出第二个(用户点的是同一个按钮)。
                if self.picking {
                    return Task::none();
                }
                self.picking = true;
                let request = self.pick_request(target);
                Task::perform(
                    async move { crate::picker::pick(request).await },
                    move |result| Message::PathPicked(target, result),
                )
            }
            Message::PathPicked(target, result) => {
                self.picking = false;
                match result {
                    // 用户按了取消:什么都不改(这不是失败)。
                    Ok(None) => Task::none(),
                    Err(e) => {
                        // 顶部错误条(它不挑页面),并顺手把按钮灰掉 —— 打不开就别再让人点。
                        self.error = Some(format!("打开文件选择框失败：{e}"));
                        self.picker = Some(Err(e));
                        Task::none()
                    }
                    Ok(Some(path)) => self.apply_picked_path(target, &path),
                }
            }
        }
    }
}

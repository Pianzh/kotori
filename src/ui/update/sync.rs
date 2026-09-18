//! 云同步那一块的消息：设置、凭据、测试连接、立即同步、恢复。
//!
//! 从 `update/mod.rs` 拆出来（那里本来是 900 多行的单个 `match`）。语义一字未动。

use super::super::*;

impl App {
    /// update_sync 负责的那一批消息。
    ///
    /// 拆出来只是因为 `update` 那个 match 太长：**这里改的仍然是同一个 `App`**，
    /// 语义一字未动。
    pub(super) fn update_sync(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SyncStatusLoaded(result) => match *result {
                Ok(status) => {
                    self.sync_form.apply(&status, &status.settings);
                    self.sync_status = Some(status);
                    Task::none()
                }
                Err(e) => {
                    self.sync_form.loaded = true;
                    self.sync_form.msg = Some(format!("读取同步状态失败: {e}"));
                    Task::none()
                }
            },
            Message::SyncToggleEnabled(value) => {
                self.sync_form.enabled = value;
                self.sync_form.settings_dirty = true;
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
                    SyncField::RcloneBinary => form.rclone_binary = value,
                    SyncField::KopiaBinary => form.kopia_binary = value,
                }
                if matches!(
                    field,
                    SyncField::Endpoint
                        | SyncField::Bucket
                        | SyncField::Prefix
                        | SyncField::KeepVersions
                        | SyncField::RcloneBinary
                        | SyncField::KopiaBinary
                ) {
                    form.settings_dirty = true;
                }
                Task::none()
            }
            Message::SyncEngineSelected(engine) => {
                // **点下去就生效**：引擎是二选一的开关，不该还要用户再去找一个「保存设置」
                // ——从前就是那样，界面上按钮立刻变成「kopia ✓」，而 config 里一个字节都
                // 没动，重开 GUI 就又回到 rclone（用户 2026-09-16 报的就是这个）。
                //
                // 只提交 engine 一个字段：用户手上那些还没保存的编辑（bucket、prefix…）
                // 一个字都不会被带上去，`settings_dirty` 在这里也**不动**。
                self.sync_form.engine = engine.clone();
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_engine(&socket, &engine).await },
                    Message::SyncEngineSaved,
                )
            }
            Message::SyncEngineSaved(result) => {
                self.sync_form.busy = false;
                match result {
                    // daemon 说这一笔真的换了引擎 ⇒ 那条"对面数据看不见"的警告要说出来。
                    Ok(true) => {
                        self.sync_form.msg = Some(engine_switched_note(&self.sync_form.engine));
                    }
                    Ok(false) => self.sync_form.msg = Some("同步方式已保存".to_string()),
                    Err(error) => {
                        self.sync_form.msg = Some(format!("切换同步方式失败: {error}"));
                        // 没写进去就别让界面继续装着已经换了：清掉 dirty，好让下面这次
                        // 刷新把配置里那个真正的值拉回来。
                        self.sync_form.settings_dirty = false;
                    }
                }
                self.reload_sync()
            }
            Message::SyncKopiaPasswordChanged(value) => {
                self.sync_form.kopia_password = value;
                Task::none()
            }
            Message::SyncSaveKopiaPassword => {
                let password = self.sync_form.kopia_password.clone();
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_kopia_password(&socket, password.trim()).await },
                    Message::SyncKopiaPasswordSaved,
                )
            }
            Message::SyncKopiaPasswordSaved(result) => {
                self.sync_form.busy = false;
                match result {
                    Ok(using_default) => {
                        // 密码进了凭据库就立刻从输入框里消失,与 B2 那两条同一套规矩。
                        self.sync_form.kopia_password.clear();
                        self.sync_form.msg = Some(if using_default {
                            "已改回默认密码 kotori —— 任何拿到这个 bucket 的人都能解开仓库"
                                .to_string()
                        } else {
                            "kopia 仓库密码已保存".to_string()
                        });
                    }
                    Err(error) => self.sync_form.msg = Some(format!("保存失败: {error}")),
                }
                self.reload_sync()
            }
            Message::SyncSaveSettings => {
                let form = self.sync_form.clone();
                self.sync_form.busy = true;
                self.sync_form.msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { save_sync_settings(&socket, form.patch()).await },
                    Message::SyncSettingsSaved,
                )
            }
            Message::SyncSettingsSaved(result) => {
                self.sync_form.busy = false;
                match &result {
                    Ok(true) => {
                        self.sync_form.msg = Some(engine_switched_note(&self.sync_form.engine));
                    }
                    Ok(false) => self.sync_form.msg = Some("已保存".to_string()),
                    Err(e) => self.sync_form.msg = Some(format!("保存失败: {e}")),
                }
                if result.is_ok() {
                    // The daemon now holds exactly what the form holds, so a
                    // later status reply may refill the form again.
                    self.sync_form.settings_dirty = false;
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
            Message::SyncTest => {
                self.sync_form.busy = true;
                // 这一条**必须**先给句话:它背后可能是一次真的网络往返(kopia 连桶、
                // 建仓库、列一次快照),最长能到几十秒,而 busy 只把按钮变灰 ——
                // 用户看到的就是"点了没反应"(2026-09-18 报的)。「立即同步全部」一直
                // 都有这句,是这一个漏了。
                self.sync_form.msg = Some("正在测试连接…".to_string());
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
            // 委派是按变体名精确列的：漏一个就会走到这里，测试会立刻炸。
            other => unreachable!("update_sync 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

//! 服务启停、wine 设置、单游戏存档位置、环境检查那一块的消息。
//!
//! 从 `update/mod.rs` 拆出来。语义一字未动。

use super::super::*;

/// 这一族消息归 [`App::update_settings`] 接。
///
/// 分派表里那一长串变体清单搬到这里，是因为**它和 handler 是一家**：新增一条设置类的
/// 消息时两个地方挨着改，不容易漏，而 `update/mod.rs` 那边只剩一行。
///
/// ⚠ 漏了的后果是掉到 `update/mod.rs` 末尾那句 `unreachable!` 上（界面一按就 panic，
/// 整页渲染测试也会立刻炸）。
pub(super) fn is_settings_message(message: &Message) -> bool {
    matches!(
        message,
        Message::ServiceStart
            | Message::ServiceStarted(..)
            | Message::ServiceStop
            | Message::ServiceStopped(..)
            | Message::WineStatusLoaded(..)
            | Message::WinePrefixChanged(..)
            | Message::SaveWinePrefix
            | Message::ClearWinePrefix
            | Message::WinePrefixSaved(..)
            | Message::GameDirChanged(..)
            | Message::GameDirDiskChanged(..)
            | Message::GameDirRelativeChanged(..)
            | Message::ExeDiskChanged(..)
            | Message::ExeRelativeChanged(..)
            | Message::MountInferred(..)
            | Message::DirectLaunchToggled(..)
            | Message::AutoWatchToggled(..)
            | Message::ProcessNameChanged(..)
            | Message::SavePathKindChanged(..)
            | Message::SavePathChanged(..)
            | Message::SavePathExcludeChanged(..)
            | Message::AddSavePath
            | Message::RemoveSavePath(..)
            | Message::Tick
            | Message::StatusLoaded(..)
            | Message::ConfigSourcePicked(..)
            | Message::ConfigSourceSwitched(..)
            | Message::EnvironmentReload
            | Message::EnvironmentLoaded(..)
    )
}

impl App {
    /// update_settings 负责的那一批消息。
    ///
    /// 拆出来只是因为 `update` 那个 match 太长：**这里改的仍然是同一个 `App`**，
    /// 语义一字未动。
    pub(super) fn update_settings(&mut self, message: Message) -> Task<Message> {
        match message {
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
            Message::DirectLaunchToggled(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.direct_launch = value;
                }
                self.schedule_auto_save()
            }
            // 自动追踪与启动方式**不互斥**(用户 2026-09-20):它只说"不是 kotori
            // 启动的那一局也要跟",开着它照样能从上面点启动。
            Message::AutoWatchToggled(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.auto_watch = value;
                }
                self.schedule_auto_save()
            }
            Message::ProcessNameChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.process_name = value;
                }
                self.schedule_auto_save()
            }
            // ⚠ 「路径」这一组**不排自动保存**（用户 2026-09-25 改的主意）：它归页面上
            // 那颗「保存路径」按钮。改路径时顺手把旧的外置盘引用清掉 —— 它已经不再指向
            // 这条路径了；想留着就自己重填，或者在盘上让 daemon 保存时重新识别一次。
            Message::GameDirChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.game_dir = value;
                    draft.game_dir_mount = MountRef::default();
                }
                Task::none()
            }
            Message::GameDirDiskChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.game_dir_mount.disk = value;
                }
                Task::none()
            }
            Message::GameDirRelativeChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.game_dir_mount.relative = value;
                }
                Task::none()
            }
            Message::ExeDiskChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.exe_mount.disk = value;
                }
                Task::none()
            }
            Message::ExeRelativeChanged(value) => {
                if let Some(draft) = &mut self.draft {
                    draft.exe_mount.relative = value;
                }
                Task::none()
            }
            // 挑完路径之后认出来的盘引用：写进草稿那两栏，按钮随之亮起（按了才落盘）。
            Message::MountInferred(for_exe, result) => {
                if let Ok(Some(mount)) = result
                    && let Some(draft) = &mut self.draft
                {
                    if for_exe {
                        draft.exe_mount = mount;
                    } else {
                        draft.game_dir_mount = mount;
                    }
                }
                Task::none()
            }
            // ⚠ 存档位置与路径同一条规矩：这里只改草稿，落盘归那颗按钮。
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
                if let Some(draft) = self.draft.as_mut()
                    && let Some(entry) = draft.save_paths.get_mut(index)
                {
                    entry.path = value.clone();
                    // 手动敲的路径也自动认 kind(用户 2026-09-19),优先级与「浏览…」
                    // 相同:相对 → 令牌 → 绝对。认不出(输入到一半)就保持原样;
                    // 改写后的令牌写法只进档案,不打断正在输入的那个框(它的文本
                    // 是页面自持的,render 只同步 kind,见 `render/detail.rs`)。
                    let game_dir = std::path::PathBuf::from(draft.game_dir.trim());
                    if let Some((kind, rewritten)) = crate::wine::infer_save_path(&game_dir, &value)
                    {
                        entry.kind = kind.as_str().to_string();
                        entry.path = rewritten;
                    }
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
                    // 默认给相对写法:它是推荐顺序的第一位(用户 2026-09-19),
                    // 输入路径后 kind 还会自动跟着文本走。
                    draft.save_paths.push(SavePathDraft {
                        kind: "relative".to_string(),
                        path: "savedata".to_string(),
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
            Message::StatusLoaded(Ok(status)) => {
                self.running = status.sessions;
                // 配置落点跟着同一次回包刷新 —— 别处切过(CLI、另一台界面)这边也跟上。
                self.config_source = status.config;
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

            // ── 配置放在哪(便携 / 平台默认) ────────────────────────────────
            Message::ConfigSourcePicked(portable) => {
                if self.config_switching {
                    return Task::none();
                }
                self.config_switching = true;
                self.config_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { set_config_source(&socket, portable).await },
                    Message::ConfigSourceSwitched,
                )
            }
            Message::ConfigSourceSwitched(result) => {
                self.config_switching = false;
                match result {
                    Ok(message) => {
                        self.config_msg = Some((message, true));
                        // 位置变了 ⇒ 重读一次状态,把新路径与按钮的可用性推回去。
                        let socket = self.daemon_socket.clone();
                        return Task::perform(
                            async move { load_status(&socket).await },
                            Message::StatusLoaded,
                        );
                    }
                    Err(e) => self.config_msg = Some((format!("切换失败: {e}"), false)),
                }
                Task::none()
            }
            Message::EnvironmentReload => Task::perform(
                async { load_environment().await },
                Message::EnvironmentLoaded,
            ),
            Message::EnvironmentLoaded(Ok(environment)) => {
                self.environment = Some(environment);
                Task::none()
            }
            Message::EnvironmentLoaded(Err(e)) => {
                // 和别处同一条规矩:「问不到」不等于「一切正常」,也不等于「有毛病」——
                // 保留上一次的结果,只记日志。
                tracing::debug!("environment report failed: {e}");
                Task::none()
            }
            // 委派是按变体名精确列的：漏一个就会走到这里，测试会立刻炸。
            other => unreachable!("update_settings 收到了不该由它处理的消息: {other:?}"),
        }
    }
}

//! `App::update`: the message loop.
//!
//! 这个 `match` 曾经有 900 多行，所以按**消息属于哪一块**拆成了几个文件：
//! 本文件管跳转、游戏库与库里的增删改，`settings` 管服务/wine/单游戏设置/环境，
//! `sync` 管云同步，「云端存档」的浏览在 `cloud`。几处改的仍然是同一个 `App`，
//! 只是每个文件不再长到读不完。

use super::*;

mod add;
mod cloud;
mod picker;
// 「启动 / 停止」那一族住在 `run.rs`;`pub(in crate::ui)` 只为单元测试叫得到它。
pub(in crate::ui) mod run;
mod settings;
mod sync;

use settings::is_settings_message;

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabChanged(tab) => {
                self.tab = tab;
                self.selected = None;
                self.draft = None;
                self.confirm_delete = false;
                self.confirm_stop = false;
                self.error = None;
                if tab == Tab::Cloud {
                    return self.cloud_entered();
                }
                if tab == Tab::Settings || tab == Tab::Sync {
                    // Re-read them all, they may have changed on disk (or in the
                    // daemon, which is the only writer).
                    let mut tasks = vec![
                        Task::perform(
                            async { load_wine_status().await },
                            Message::WineStatusLoaded,
                        ),
                        Task::perform(async { load_sync_status().await }, |result| {
                            Message::SyncStatusLoaded(Box::new(result))
                        }),
                    ];
                    // 环境检查只在设置页问:它会真去跑几个外部程序(见 `platform`)。
                    if tab == Tab::Settings {
                        tasks.push(Task::perform(
                            async { load_environment().await },
                            Message::EnvironmentLoaded,
                        ));
                    }
                    return Task::batch(tasks);
                }
                Task::none()
            }
            // 「从正在运行的进程里挑」那一族消息在 `update/picker.rs`。
            m @ (Message::ProcessPickerOpen
            | Message::ProcessesLoaded(..)
            | Message::ProcessQueryChanged(..)
            | Message::ProcessPicked(..)
            | Message::ProcessPickerClose) => self.update_picker(m),
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
            // 一颗按钮两种时候:该启动还是该停由 `run_action` 说了算(文案也从同一
            // 份会话表来,所以两者不可能再说两套话)。
            Message::ToggleRun(game_id) => self.toggle_run(game_id),
            Message::LaunchDone(result) => {
                // 自检认不出云端那一条：daemon 先没起游戏，把问题交回来了。这一问由
                // 浮层接（`SyncAskState`），回答走 `SyncAskAnswered`。
                if result
                    .as_ref()
                    .is_ok_and(|value| value["needs_sync_decision"] == serde_json::json!(true))
                {
                    self.sync_ask = self.launching.take();
                    self.error = None;
                    return Task::none();
                }
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
                    // 二次确认属于上一款,别带过来。
                    self.confirm_stop = false;
                    // Seed the form from the *stored* profile. Anything else
                    // means a plain "open + save" silently rewrites settings.
                    self.draft = Some(Draft::from_game(g));
                    // 这一页现在也显示这个游戏的云存档状况,而 `sync.status` 平时只在
                    // 打开「云同步」/「设置」页时才读 —— 直接从游戏库点进来时补一次。
                    load_sync = self.sync_status.is_none();
                }
                if load_sync {
                    return Task::perform(async { load_sync_status().await }, |result| {
                        Message::SyncStatusLoaded(Box::new(result))
                    });
                }
                Task::none()
            }
            Message::BackToList => {
                self.selected = None;
                self.draft = None;
                self.saved_msg = None;
                self.confirm_delete = false;
                self.confirm_stop = false;
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
            Message::LaunchArgsChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.launch_args = v;
                }
                self.schedule_auto_save()
            }
            Message::GamescopeArgsChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.gamescope_args = v;
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
            m @ (Message::NewNameChanged(..)
            | Message::NewGameDirChanged(..)
            | Message::NewExeChanged(..)
            | Message::CreateRequested
            | Message::CreateFinished(..)
            | Message::MatchExeReady(..)
            | Message::MatchLoaded(..)
            | Message::MatchChoose(..)
            | Message::MatchDecline
            | Message::MatchUndoDecline
            | Message::MatchClearPick
            | Message::CloudPickOpen
            | Message::CloudPickLoaded(..)
            | Message::CloudPickSearch(..)
            | Message::CloudPickChoose(..)
            | Message::CloudPickDismiss
            | Message::GamePaired(..)) => self.update_add(m),
            // ── 云同步（处理在 `update::update_sync`） ──
            m @ (Message::SyncStatusLoaded(..)
            | Message::SyncToggleEnabled(..)
            | Message::SyncField(..)
            | Message::SyncEngineSelected(..)
            | Message::SyncEngineSaved(..)
            | Message::SyncKopiaPasswordChanged(..)
            | Message::SyncSaveKopiaPassword
            | Message::SyncKopiaPasswordSaved(..)
            | Message::SyncSaveSettings
            | Message::SyncSettingsSaved(..)
            | Message::SyncSaveCredentials
            | Message::SyncCredentialsSaved(..)
            | Message::SyncClearCredentials
            | Message::SyncCredentialsCleared(..)
            | Message::SyncTest
            | Message::SyncTested(..)
            | Message::SyncScanCloud
            | Message::SyncPairingScanned(..)
            | Message::SyncPair(..)
            | Message::SyncPaired(..)
            | Message::SyncRejectPairing(..)
            | Message::SyncPairingRejected(..)
            | Message::SyncMasterPasswordChanged(..)
            | Message::SyncUnlock
            | Message::SyncUnlocked(..)
            | Message::SyncSetMasterPassword
            | Message::SyncMasterSaved(..)
            | Message::SyncLockCredentials
            | Message::SyncCredentialsLocked(..)
            | Message::SyncMasterDeleteRequested
            | Message::SyncMasterDeleteCancelled
            | Message::SyncMasterDeleteConfirmed
            | Message::SyncMasterDeleted(..)
            | Message::SyncNow(..)
            | Message::SyncNowDone(..)
            | Message::SyncRestoreRequested(..)
            | Message::SyncRestoreCancelled
            | Message::SyncRestoreConfirmed) => self.update_sync(m),

            // ── 「云端存档」的浏览（处理在 `update::update_cloud`） ──
            // 它只读云端、一个字都不改本机配置，所以与上面那一族分开列。
            m @ (Message::CloudRefresh
            | Message::CloudLoaded(..)
            | Message::CloudScan
            | Message::CloudScanned(..)
            | Message::CloudSearch(..)
            | Message::CloudToggle(..)
            | Message::CloudBack
            | Message::CloudVersionsLoaded(..)) => self.update_cloud(m),

            // ── 服务、wine 与单游戏设置（处理在 `update::update_settings`） ──
            // 这一族有哪些变体由 `is_settings_message` 说了算（它就在 handler 旁边，
            // 两处挨着改，不会漏）。
            m if is_settings_message(&m) => self.update_settings(m),
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
            // 二次确认那一步里用户按了「取消」:什么都不做。
            Message::SyncAskAnswered(choice) => self.sync_ask_answered(choice),
            Message::SyncAskDismissed => {
                // 取消 = 这一次不启动（用户可能想去配对表看一眼再回来）。
                self.sync_ask = None;
                self.launching = None;
                Task::none()
            }
            Message::StopCancelled => {
                self.confirm_stop = false;
                Task::none()
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
                        // 这一次没成而已:顶部错误条说一句就够了。
                        // ⚠ **不许**动 `self.picker` —— 它说的是"这台机器上有没有文件对话框",
                        //    是开机探出来的结论。一次失败(何况用户取消)不代表它从此没有了:
                        //    真机上点一次叉号就把「浏览…」永久灰掉了(用户 2026-09-13 报的),
                        //    原因正是这里曾把它写成 `Some(Err(e))`。
                        tracing::warn!("打开文件选择框失败：{e}");
                        self.error = Some(format!("打开文件选择框失败：{e}"));
                        Task::none()
                    }
                    Ok(Some(path)) => self.apply_picked_path(target, &path),
                }
            }
            // 每一族都按变体精确列了，所以走到这里只可能是"新加了一条消息而忘了挂到
            // 某一族"。与其静默丢掉它，不如立刻炸出来 —— 整页渲染测试会先撞上。
            other => unreachable!("没有 handler 认领这条消息: {other:?}"),
        }
    }
}

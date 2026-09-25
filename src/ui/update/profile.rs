//! 单游戏设置页那些"改一笔就自动保存"的消息。
//!
//! 十二个分支长得一模一样:把值写进草稿的某个字段,然后安排一次防抖保存。它们原来
//! 挤在 `update/mod.rs` 的 match 里,而那一刀把 `mod.rs` 顶过了 500 行的硬线
//! (AGENTS.md),于是整族搬出来。
//!
//! ⚠ 这一族**只碰草稿**:真正落盘是 [`App::schedule_auto_save`] 之后的事;而"切了
//! 游戏之后那笔怎么补发"就在本文件的 `profile_saved` 里(见 BUG-18)。

use super::super::*;

impl App {
    /// 更新草稿里那一个字段,然后安排一次防抖保存。
    ///
    /// 调用方(`update/mod.rs` 的 match)只把这一族消息递进来 —— 认不出的那些回
    /// `Task::none()`,什么都不做。
    pub(super) fn update_profile_edit(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AlgoChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.algo = v;
                }
            }
            Message::SharpnessChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.sharpness = v.round() as u32;
                }
            }
            Message::InternalWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_w = v;
                }
            }
            Message::InternalHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_h = v;
                }
            }
            Message::OutputWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_w = v;
                }
            }
            Message::OutputHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_h = v;
                }
            }
            Message::ScaleRatioChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.scale_ratio = v;
                }
            }
            Message::FullscreenToggled(v) => {
                if let Some(d) = &mut self.draft {
                    d.fullscreen = v;
                }
            }
            Message::FramerateChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.framerate = v;
                }
            }
            Message::ExePathChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.exe = v;
                    // 路径改了，旧的外置盘引用跟着作废（与 `update::settings` 里
                    // `GameDirChanged` 同一条理由：它已经不再指向这条路径了）。
                    d.exe_mount = MountRef::default();
                }
                // ⚠ 路径那一组归页面上的「保存路径」按钮，**不排自动保存** —— 提前返回，
                // 别掉进下面那句统一的 `schedule_auto_save()`。
                return Task::none();
            }
            Message::LaunchArgsChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.launch_args = v;
                }
            }
            Message::GamescopeArgsChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.gamescope_args = v;
                }
            }
            // 调用方只递这一族;真走到这里,说明 `mod.rs` 那条 `m @ (...)` 漏了它。
            _ => return Task::none(),
        }
        self.schedule_auto_save()
    }
    /// 一笔保存的**回包**：推进书签、报一句话、必要时补发，最后把库读一遍。
    ///
    /// 从 `update/mod.rs` 搬进来（照这个文件头说的）：这一族"改一笔就存"的字段都在这里，
    /// 回包与它们是一件事 —— BUG-18 那条"过期就补发"就住在这个函数里。
    pub(super) fn profile_saved(
        &mut self,
        generation: u64,
        result: Result<(), String>,
    ) -> Task<Message> {
        let Some(attempt) = self.save_in_flight.take() else {
            // 一笔只回一次,理论上到不了这儿;真到了也别让界面永远停在"保存中"。
            self.saving = false;
            return Task::none();
        };
        self.saving = false;
        // 写的是哪一组由**那一笔自己**说了算（回包里不带，见 `Message`）。
        let scope = attempt.scope;
        // 用户可能已经翻到别的游戏去了:回包只能落在它自己那一份草稿上,
        // 否则会把别人的 `*_original` 写成这个游戏的值。
        let same_game = self.selected.as_deref() == Some(attempt.draft.game_id.as_str());

        match &result {
            Ok(()) => {
                if same_game {
                    if let Some(draft) = self.draft.as_mut() {
                        // 服务端现在有的就是这一笔带过去的东西 ⇒ 把**这一组**
                        // 的书签推进过去,下一次只发改过的字段(游戏盘没挂载时
                        // 也不会因为重发旧路径而白报错)。另外两组没动过,别碰。
                        match scope {
                            SaveScope::Auto => {}
                            SaveScope::Paths => {
                                draft.game_dir_original = attempt.draft.game_dir.clone();
                                draft.exe_original = attempt.draft.exe.clone();
                                draft.game_dir_mount_original =
                                    attempt.draft.game_dir_mount.clone();
                                draft.exe_mount_original = attempt.draft.exe_mount.clone();
                            }
                            SaveScope::Saves => {
                                draft.save_paths_original = attempt.draft.save_paths.clone();
                            }
                        }
                    }
                    self.report_saved(
                        if scope == SaveScope::Auto {
                            "已自动保存"
                        } else {
                            "已保存"
                        },
                        true,
                    );
                }
            }
            Err(e) => {
                // 草稿一个字都不动:用户正在打的那半截不能被回包吃掉。
                if same_game {
                    self.report_saved(format!("保存失败: {e}"), false);
                } else {
                    // 已经离开那一页了,别把失败吞掉 —— 挂到顶部的错误条上。
                    self.error = Some(format!("保存失败: {e}"));
                }
            }
        }

        // 这一笔已经过期(按过「重置」、又改过,或者用户已经翻到别的游戏去了):
        // 配置里现在写着的可能是一个用户不要的值,用手上的草稿再存一次把它拉
        // 回来。⚠ **不能只看 `same_game`**:切到另一款之后,新款那笔编辑会因为
        // "上一笔还在路上"被退回,这里若不补发就再也没人发它了 —— B 的修改就是
        // 这样丢的(BUG-18)。
        //
        // ⚠ **只有自动那一族才补发**:路径与存档位置是按钮驱动的,补发等于又把它
        // 变回自动保存(用户 2026-09-25 明确不要那个)。
        if scope == SaveScope::Auto && generation != self.autosave_generation {
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

    /// 「重置」：作废挂在防抖窗口里的那一笔，再按「已存值」把页面铺一遍。
    pub(super) fn reset_profile(&mut self) -> Task<Message> {
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
}

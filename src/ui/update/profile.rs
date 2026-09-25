//! 单游戏设置页那些"改一笔就自动保存"的消息。
//!
//! 十二个分支长得一模一样:把值写进草稿的某个字段,然后安排一次防抖保存。它们原来
//! 挤在 `update/mod.rs` 的 match 里,而那一刀把 `mod.rs` 顶过了 500 行的硬线
//! (AGENTS.md),于是整族搬出来。
//!
//! ⚠ 这一族**只碰草稿**:真正落盘是 [`App::schedule_auto_save`] 之后的事;而"切了
//! 游戏之后那笔怎么补发"在 `update/mod.rs` 的 `ProfileSaved` 里(见 BUG-18)。

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
}

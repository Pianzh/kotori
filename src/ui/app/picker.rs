//! 从系统对话框挑回来的路径怎么落到各处。
//!
//! 单游戏页那几个输入框由**页面自己**持有（见 `game-settings.slint` 的文件头），所以挑完
//! 除了改草稿，还要记一笔"这次填的是哪个框"，`render` 靠它推一次；另外顺手问 daemon
//! 这条路径落在哪块盘上，把「盘号 / 相对目录」两栏填好。

use super::*;

impl App {
    /// 对话框里挑回来的路径,填进对应的那个框。
    ///
    /// 单游戏设置页的目标还要多做两件事:值写进 `draft`(⚠ 路径那一组**不自动保存**,
    /// 只有「保存路径」按钮能落盘),并记下这次填的是什么 —— 那些输入框由**页面自己**
    /// 持有,Rust 平时不往里写(见 `game-settings.slint` 的文件头),所以 `render` 要靠
    /// 这条记录推一次。挑完还会认一次盘,把「盘号 / 相对目录」两栏填好。
    pub(in crate::ui) fn apply_picked_path(
        &mut self,
        target: PathTarget,
        picked: &Path,
    ) -> Task<Message> {
        let text = picked.display().to_string();

        match target {
            PathTarget::NewGameDir => {
                self.new_game_dir = text;
                // 挑完顺手认一次盘：在盘上就把「盘号 / 相对目录」填好，让用户在点
                // 「添加游戏」**之前**看得见（用户 2026-09-25 定的"中间加一小步"）。
                return self.infer_new_mount(false);
            }
            // 浏览 exe 也必须触发联动 —— 走 `set_new_exe`,别直接赋值(见那里的说明);
            // 顺带排一次云端匹配(手打那条路走的是 `NewExeChanged`),再认一次盘。
            PathTarget::NewExe => {
                self.set_new_exe(text);
                return Task::batch([self.schedule_match(), self.infer_new_mount(true)]);
            }
            PathTarget::WinePrefix => {
                self.wine_prefix_input = text;
                // 用户亲手选的路径不许被随后回来的 `wine.status` 盖掉。
                self.wine_prefix_dirty = true;
            }
            // 两个"程序位置"是 `[sync]` 里的设置项，所以它们和 bucket 那些一样是**表单
            // 的一部分**（随「保存设置」一起提交），只是另有一个浏览按钮帮着填。
            PathTarget::RcloneBinary => {
                self.sync_form.rclone_binary = text;
                self.sync_form.settings_dirty = true;
            }
            PathTarget::KopiaBinary => {
                self.sync_form.kopia_binary = text;
                self.sync_form.settings_dirty = true;
            }
            PathTarget::GameDir | PathTarget::Exe => {
                let Some(draft) = self.draft.as_mut() else {
                    return Task::none();
                };
                if target == PathTarget::GameDir {
                    draft.game_dir = text.clone();
                    // 换路径了，旧的外置盘引用跟着作废（同 `GameDirChanged`）。
                    draft.game_dir_mount = MountRef::default();
                } else {
                    draft.exe = text.clone();
                    draft.exe_mount = MountRef::default();
                }
                self.picked_path = Some((target, text));
                // ⚠ 路径那一组归「保存路径」按钮，**不排自动保存**；这里只认一次盘，
                // 把那两栏填好给用户看。
                return self.infer_draft_mount(target == PathTarget::Exe);
            }
            PathTarget::SavePath(index) => {
                // 挑回来的路径**自己**说明它属于哪一类(相对 → 令牌 → 绝对),
                // 所以这里顺手把类型也改对,再按那一类翻译。
                //
                // 从前是拿**当前选中的类型**去翻译,类型与路径不符就报错、什么都不改 ——
                // 而真 Windows 上挑回来的必然是 `C:\Users\…`,当时那个 windows 分支只认
                // wine 的 `drive_c/users/…` 形状,于是"点浏览没反应"(BUG-REPORT
                // 「存档位置的浏览有问题」)。顺序与取舍见 `wine::portable_save_path`。
                let (kind, value) = {
                    let Some(draft) = self.draft.as_ref() else {
                        return Task::none();
                    };
                    if draft.save_paths.get(index).is_none() {
                        return Task::none();
                    }
                    let (kind, text) =
                        crate::wine::portable_save_path(Path::new(draft.game_dir.trim()), picked);
                    (kind.as_str().to_string(), text)
                };
                if let Some(entry) = self
                    .draft
                    .as_mut()
                    .and_then(|draft| draft.save_paths.get_mut(index))
                {
                    entry.kind = kind;
                    entry.path = value.clone();
                }
                self.picked_path = Some((target, value));
                // ⚠ 存档位置归那一组自己的保存按钮，挑回来的路径只进草稿。
                return Task::none();
            }
        }
        Task::none()
    }

    /// 挑完一条路径之后认一次盘，把**添加页**那两栏（盘号 / 相对目录）填好 ——
    /// 用户 2026-09-25 定的"中间加一小步"：在点「添加游戏」之前就看得见。
    /// 认不出来（不在任何挂载盘上、或者压根没这块盘）什么都不动，那不是错误。
    pub(in crate::ui) fn infer_new_mount(&mut self, for_exe: bool) -> Task<Message> {
        let path = if for_exe {
            self.new_exe.trim().to_string()
        } else {
            self.new_game_dir.trim().to_string()
        };
        if path.is_empty() {
            return Task::none();
        }
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { mount_infer(&socket, &path).await },
            move |result| Message::NewMountInferred(for_exe, result),
        )
    }

    /// 编辑页的同一步：认出来的引用写进草稿那两栏（按钮随之亮起，按了才落盘）。
    pub(in crate::ui) fn infer_draft_mount(&mut self, for_exe: bool) -> Task<Message> {
        let Some(draft) = self.draft.as_ref() else {
            return Task::none();
        };
        let path = if for_exe {
            draft.exe.trim().to_string()
        } else {
            draft.game_dir.trim().to_string()
        };
        if path.is_empty() {
            return Task::none();
        }
        let socket = self.daemon_socket.clone();
        Task::perform(
            async move { mount_infer(&socket, &path).await },
            move |result| Message::MountInferred(for_exe, result),
        )
    }
}

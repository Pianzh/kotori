//! 「从正在运行的进程里挑」那一族消息。
//!
//! 从 `update/mod.rs` 拆出来(那边已经 500 行):两个入口共用同一个浮层,差别只在
//! 挑完那一下 —— [`PickerPurpose`] 就是那个差别。

use super::super::*;

impl App {
    /// update_picker 负责的那一批消息。
    pub(super) fn update_picker(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ProcessPickerOpen(purpose) => {
                self.process_picker.open(purpose);
                let socket = self.daemon_socket.clone();
                Task::perform(async move { load_processes(&socket).await }, |result| {
                    Message::ProcessesLoaded(result)
                })
            }
            Message::ProcessesLoaded(result) => {
                match result {
                    Ok(rows) => self.process_picker.loaded(rows),
                    Err(e) => self.process_picker.failed(e),
                }
                Task::none()
            }
            Message::ProcessQueryChanged(query) => {
                self.process_picker.set_query(query);
                Task::none()
            }
            Message::ProcessPickerClose => {
                self.process_picker.close();
                Task::none()
            }
            Message::ProcessPicked(index) => self.process_picked(index),
            other => unreachable!("update_picker 收到了不该由它处理的消息: {other:?}"),
        }
    }

    /// 挑了「从运行中的进程里挑」浮层里的一行:两个入口各做各的事
    /// (详情页 = 立刻开始跟这一局;添加游戏页 = 把那三个框替用户填好)。
    fn process_picked(&mut self, index: usize) -> Task<Message> {
        let Some(row) = self.process_picker.row(index).cloned() else {
            return Task::none();
        };
        let purpose = self.process_picker.purpose();
        self.process_picker.close();

        match purpose {
            PickerPurpose::FollowPid => {
                // 填进那一栏(用户看得见跟的是哪一个),然后立刻开始跟 —— 挑一次就够,
                // 不该再让人去点一下「跟这一局」。
                self.follow_pid_input = row.pid.to_string();
                let Some(game_id) = self.selected.clone() else {
                    return Task::none();
                };
                self.following = true;
                self.saved_msg = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { observe_process(&socket, &game_id, row.pid).await },
                    Message::FollowDone,
                )
            }
            PickerPurpose::NewGame => {
                if row.exe.is_empty() {
                    // 拿不到路径就没法建条目(Linux 上非 Z: 盘符的 wine 进程就是这样)。
                    self.create_msg = Some(format!(
                        "这个进程（{}，PID {}）拿不到 exe 路径，换一个挑,或者手动填。",
                        row.name, row.pid
                    ));
                    return Task::none();
                }
                // 填进「可执行文件」——它顺带把根目录与名字也填好(见 `set_new_exe`)。
                self.set_new_exe(row.exe.clone());
                // 窗口标题比 exe 文件名认得出(「BLACKSOULS Ⅱ」vs「Game」),有就用它,
                // 之后用户想改随时改。
                if !row.title.is_empty() {
                    self.new_name = row.title.clone();
                }
                self.create_msg = Some(format!(
                    "已按进程 {} 填好（PID {}），确认后点「添加游戏」",
                    row.name, row.pid
                ));
                Task::none()
            }
        }
    }
}

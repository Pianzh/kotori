//! 「从正在运行的进程里挑」那一族消息。
//!
//! 从 `update/mod.rs` 拆出来(那边已经 500 行):这一族只服务一个入口 —— 添加游戏页
//! 那颗「从运行中的进程添加」,挑完就把名字与 exe 替用户填好。
//!
//! ⚠ 从前这里还有第二个入口(详情页「跟当前这一局(PID)」)。**PID 那条路已经删掉**
//! 了(用户 2026-09-20:"自动跟踪的 pid 填写实质上意义不大"):自动追踪现在按 exe 的
//! 完整路径认人,不再需要"只跟这一次"的特例。

use super::super::*;

impl App {
    /// update_picker 负责的那一批消息。
    pub(super) fn update_picker(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ProcessPickerOpen => {
                self.process_picker.open();
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

    /// 挑了浮层里的一行:把那三个框替用户填好(exe、根目录、名字)。
    fn process_picked(&mut self, index: usize) -> Task<Message> {
        let Some(row) = self.process_picker.row(index).cloned() else {
            return Task::none();
        };
        self.process_picker.close();

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
        // 填好 exe 之后照手打那条路走一遍:问一次云端有没有这一款(见 `model::add`)；
        // 顺带认一次盘，把「盘号 / 相对目录」两栏填好（用户 2026-09-25 定的"中间加一小步"）。
        Task::batch([self.schedule_match(), self.infer_new_mount(true)])
    }
}

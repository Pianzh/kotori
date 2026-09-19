//! 「添加游戏」页的消息:三个输入框、exe 联动的自动填充,以及提交与回包。
//!
//! 从 `update/mod.rs` 拆出来(那边曾越过 500 行硬线)。这里改的仍然是同一个
//! `App`,只是这批消息住在这一个文件里。

use super::super::*;

impl App {
    /// update_add 负责的那一批消息。
    pub(super) fn update_add(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::NewNameChanged(value) => {
                self.new_name = value;
                Task::none()
            }
            Message::NewGameDirChanged(value) => {
                self.new_game_dir = value;
                Task::none()
            }
            Message::NewExeChanged(value) => {
                self.new_exe = value.clone();
                // exe 一变,根目录与名字跟着自动填(用户 2026-09-19):根目录 = exe
                // 所在目录,名字 = exe 文件名。字段为空、或者还等于上次自动填的值
                // (= 用户没动过)才重填;用户自己改过的值不动。
                let (dir, name) = autofill_from_exe(&value);
                if let Some(dir) = &dir
                    && (self.new_game_dir.is_empty() || self.new_game_dir == self.auto_filled_dir)
                {
                    self.new_game_dir = dir.clone();
                }
                self.auto_filled_dir = dir.unwrap_or_default();
                if let Some(name) = &name
                    && (self.new_name.is_empty() || self.new_name == self.auto_filled_name)
                {
                    self.new_name = name.clone();
                }
                self.auto_filled_name = name.unwrap_or_default();
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
                    Ok((id, warning)) => {
                        // 重复 exe 不挡添加,但要让用户看到那三条隐患(云端版本
                        // 历史劈半 / 同名观测混淆 / 并发写同一存档目录)。
                        let mut message = format!("已添加（ID: {id}），可在游戏库里继续配置");
                        if let Some(text) = warning {
                            message.push_str("\n\n");
                            message.push_str(&text);
                        }
                        self.create_msg = Some(message);
                        self.new_name.clear();
                        self.new_game_dir.clear();
                        self.new_exe.clear();
                        self.auto_filled_dir.clear();
                        self.auto_filled_name.clear();
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            _ => unreachable!("update_add 只接添加游戏那批消息"),
        }
    }
}

/// 从 exe 文本推出自动填充的 `(根目录, 默认名字)`:名字是文件名去掉最后一个
/// 扩展名(`"game.exe"` → `"game"`)。
///
/// 按"两种分隔符都认"手动切,不走 `Path`:反斜杠在 Linux 的 `Path` 里不是分隔符,
/// 整条 Windows 路径会被当成单个文件名,而添加页在两个平台上都要能用。
/// exe 还没填、或者只是个裸文件名时,根目录给 `None`(不猜)。
fn autofill_from_exe(exe: &str) -> (Option<String>, Option<String>) {
    fn stem_of(file: &str) -> Option<String> {
        let stem = match file.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => stem,
            _ => file,
        };
        (!stem.is_empty()).then(|| stem.to_string())
    }

    let exe = exe.trim();
    if exe.is_empty() {
        return (None, None);
    }
    let trimmed = exe.trim_end_matches(['/', '\\']);
    match trimmed.rsplit_once(['/', '\\']) {
        Some(("", file)) => (None, stem_of(file)),
        Some((dir, file)) => (Some(dir.to_string()), stem_of(file)),
        None => (None, stem_of(trimmed)),
    }
}

#[cfg(test)]
mod autofill_tests {
    use super::autofill_from_exe;

    #[test]
    fn exe_fills_dir_and_name_on_both_platforms() {
        // Windows 形状(反斜杠,Linux 的 Path 解析不了它)。
        let (dir, name) = autofill_from_exe(r"F:\Games\Hoshi\game.exe");
        assert_eq!(dir.as_deref(), Some(r"F:\Games\Hoshi"));
        assert_eq!(name.as_deref(), Some("game"));
        // Unix 形状。
        let (dir, name) = autofill_from_exe("/games/demo/3days_chs.exe");
        assert_eq!(dir.as_deref(), Some("/games/demo"));
        assert_eq!(name.as_deref(), Some("3days_chs"));
        // 多个点只去最后一个扩展名。
        let (_, name) = autofill_from_exe(r"C:\x\Game.v1.2.exe");
        assert_eq!(name.as_deref(), Some("Game.v1.2"));
        // 裸文件名:有名字,不猜目录。
        let (dir, name) = autofill_from_exe("game.exe");
        assert_eq!(dir, None);
        assert_eq!(name.as_deref(), Some("game"));
        // 空的什么都不填。
        assert_eq!(autofill_from_exe(""), (None, None));
        assert_eq!(autofill_from_exe("   "), (None, None));
    }
}

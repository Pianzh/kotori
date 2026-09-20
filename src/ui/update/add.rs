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
                self.set_new_exe(value);
                Task::none()
            }
            Message::CreateRequested => {
                let exe = self.new_exe.trim().to_string();
                if exe.is_empty() {
                    self.create_msg = Some("可执行文件必须填写".to_string());
                    return Task::none();
                }
                // 根目录与游戏名都可以不填 —— 不填就按 exe 自己推(用户 2026-09-19
                // 「不填时自动填充」)。浏览回来的路径早就在 `set_new_exe` 里填过一遍,
                // 这里是"用户从头到尾没碰过那两个框"时的兜底。
                let (dir, stem) = autofill_from_exe(&exe);
                let name = match self.new_name.trim() {
                    "" => stem.unwrap_or_default(),
                    typed => typed.to_string(),
                };
                if name.is_empty() {
                    self.create_msg =
                        Some("游戏名填不上:路径里看不出文件名,请自己写一个".to_string());
                    return Task::none();
                }
                let game_dir = match self.new_game_dir.trim() {
                    "" => dir.unwrap_or_default(),
                    typed => typed.to_string(),
                };
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

    /// exe 那一栏被写入了新值 —— **手打和「浏览…」挑回来都走这里**。
    ///
    /// 从前只有手打走联动(消息 `NewExeChanged`),而浏览是从 `app.rs` 的
    /// `apply_picked_path` 直接赋值进来的,**两条路一个填一个不填** —— 用户点了
    /// 浏览反而看不到下面两个框被填好(用户 2026-09-20 实测反馈)。现在两条路
    /// 共用这一个入口,想漏也漏不掉。
    ///
    /// 联动规则:根目录 = exe 所在目录,名字 = exe 文件名。字段为空、或者还等于
    /// 上次自动填的值(= 用户没动过)才重填;用户自己改过的值不动。
    pub(in crate::ui) fn set_new_exe(&mut self, value: String) {
        self.new_exe = value;
        let (dir, name) = autofill_from_exe(&self.new_exe);
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

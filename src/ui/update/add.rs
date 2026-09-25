//! 「添加游戏」页的消息:三个输入框、exe 联动的自动填充、**填完 exe 之后的云端匹配**,
//! 以及提交与回包。
//!
//! 从 `update/mod.rs` 拆出来(那边曾越过 500 行硬线)。这里改的仍然是同一个
//! `App`,只是这批消息住在这一个文件里。
//!
//! 匹配那一块的口径(用户 2026-09-23 定的):exe 是唯一必填项;写完 exe 就去云端认这一款,
//! 认出来就在**本页**直接确定。认领走的是已有的 `sync.pair`(身份/落点唯一的写入口),
//! 而这一块**绝不许挡住添加** —— 问不成只是页面上的一句话。

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
                self.schedule_match()
            }
            Message::NewGameDirDiskChanged(value) => {
                self.new_game_dir_disk = value;
                Task::none()
            }
            Message::NewGameDirRelativeChanged(value) => {
                self.new_game_dir_relative = value;
                Task::none()
            }
            Message::NewExeDiskChanged(value) => {
                self.new_exe_disk = value;
                Task::none()
            }
            Message::NewExeRelativeChanged(value) => {
                self.new_exe_relative = value;
                Task::none()
            }
            // 挑完一条路径之后认一次盘：认得出来就把那两栏填好，让用户在点「添加游戏」
            // **之前**看得见（用户 2026-09-25 定的"中间加一小步"）。认不出来（不在任何
            // 挂载盘上、或压根没有这块盘）就什么都不动 —— 那不是错误，只是没有引用可用。
            Message::NewMountInferred(for_exe, result) => {
                if let Ok(Some(mount)) = result {
                    if for_exe {
                        self.new_exe_disk = mount.disk;
                        self.new_exe_relative = mount.relative;
                    } else {
                        self.new_game_dir_disk = mount.disk;
                        self.new_game_dir_relative = mount.relative;
                    }
                }
                Task::none()
            }
            // 防抖到点:这期间用户又改了的话,这一个定时器就作废(他还会再排一个)。
            Message::MatchExeReady(exe) => {
                if !self.add_match.still_pending(&exe) {
                    return Task::none();
                }
                self.add_match.asking(&exe);
                let socket = self.daemon_socket.clone();
                let asked = exe.clone();
                Task::perform(
                    async move { match_exe(&socket, asked).await },
                    move |result| Message::MatchLoaded(exe, result),
                )
            }
            Message::MatchLoaded(exe, result) => {
                match result {
                    Ok((indexed, rows)) => self.add_match.loaded(&exe, indexed, rows),
                    // 问不成不是错误状态:页面上照旧能点「添加游戏」(见 `model::add`)。
                    Err(e) => self.add_match.failed(&exe, e),
                }
                Task::none()
            }
            Message::MatchChoose(cloud_id) => {
                self.add_match.choose(&cloud_id);
                Task::none()
            }
            Message::MatchDecline => {
                self.add_match.decline();
                Task::none()
            }
            Message::MatchUndoDecline => {
                self.add_match.undo_decline();
                Task::none()
            }
            // 「自己选…」:打开浮层,读一次云端清单 —— 只读**本机缓存**(用户 2026-09-24:
            // "其他所有查询都只查本地索引";本地还没有缓存时 daemon 会下载一次,
            // 见 `cloud_index_view`)。
            Message::CloudPickOpen(purpose) => {
                self.cloud_pick.open(purpose);
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { cloud_list(&socket, false).await },
                    Message::CloudPickLoaded,
                )
            }
            Message::CloudPickLoaded(result) => {
                match result {
                    Ok(reply) => self.cloud_pick.loaded(reply),
                    // 读不成不是错误状态:浮层里说一句话,**这一款照旧能添加**。
                    Err(e) => self.cloud_pick.failed(e),
                }
                Task::none()
            }
            Message::CloudPickSearch(query) => {
                self.cloud_pick.set_query(query);
                Task::none()
            }
            Message::CloudPickChoose(cloud_id) => {
                // 按 `cloud_id` 从**全量**里取(见 `CloudPick::pick`):搜索词怎么变都不会认错人。
                let purpose = self.cloud_pick.purpose();
                let Some(row) = self.cloud_pick.pick(&cloud_id) else {
                    return Task::none();
                };
                match purpose {
                    // 添加页:挑中的那条成为这一款的云端身份(建完之后 `sync.pair`)。
                    CloudPickPurpose::Add => self.add_match.pick(row),
                    // 启动前那一问:挑中的就是"这一款在云端是谁",挑完接着启动。
                    CloudPickPurpose::Launch => {
                        return self.sync_ask_pair_with(row.cloud_id, row.cloud_key);
                    }
                    // 单游戏页换绑:挑中的那条成为新的绑定,**不**启动游戏。
                    CloudPickPurpose::Rebind => {
                        return self.change_binding(Some((row.cloud_id, row.cloud_key)));
                    }
                }
                Task::none()
            }
            Message::CloudPickDismiss => {
                let purpose = self.cloud_pick.purpose();
                self.cloud_pick.close();
                // 启动前那一问被关掉(点空白/关闭)= 关掉这一款并**照常启动**
                // (用户 2026-09-24:"被关掉就直接关掉云同步即可,不妨碍游戏正常启动")。
                if purpose == CloudPickPurpose::Launch {
                    return self.sync_ask_declined();
                }
                Task::none()
            }
            // 浮层底部那颗「这一款要新建一条云端身份」：收起浮层、进入确认
            // （确认画在单游戏页那一块里，见 `pages/game-sync.slint`）。
            Message::CloudPickNewIdentity => {
                self.cloud_pick.close();
                self.sync_new_pending = true;
                Task::none()
            }
            Message::MatchClearPick => {
                self.add_match.clear_picked();
                Task::none()
            }
            Message::GamePaired(result) => {
                // 添加已经成功了 —— 这一句只是补一句"云端那边怎么样了"(失败不算添加失败)。
                let outcome = match result {
                    Ok(()) => "已与云端绑定。".to_string(),
                    Err(e) => {
                        format!("云端绑定没做成:{e}(以后可以在「云端存档」页配对)")
                    }
                };
                let message = self.create_msg.take().unwrap_or_default();
                self.create_msg = Some(format!("{message}\n{outcome}"));
                Task::none()
            }
            Message::CreateRequested => {
                let exe = self.new_exe.trim().to_string();
                let exe_mount = MountRef {
                    disk: self.new_exe_disk.trim().to_string(),
                    relative: self.new_exe_relative.trim().to_string(),
                };
                let dir_mount = MountRef {
                    disk: self.new_game_dir_disk.trim().to_string(),
                    relative: self.new_game_dir_relative.trim().to_string(),
                };
                // 既没有路径、也没有引用 = 这条档案无从启动。给了引用就放行：盘可能
                // 插在别的机器上（用户 2026-09-25："大不了就是报错打不开，这是正常的"）。
                if exe.is_empty() && exe_mount.is_unset() {
                    self.create_msg =
                        Some("可执行文件必须填写（或者填上它所在外置盘的盘号）".to_string());
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
                    async move {
                        create_game(&socket, name, exe, game_dir, &dir_mount, &exe_mount).await
                    },
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
                        // 绑定要在清场之前取(清场会把匹配状态一起收掉)。
                        let binding = self.add_match.binding().map(|row| {
                            (
                                row.cloud_key.clone(),
                                row.cloud_id.clone(),
                                row.name.clone(),
                            )
                        });
                        self.new_name.clear();
                        self.new_game_dir.clear();
                        self.new_exe.clear();
                        self.new_game_dir_disk.clear();
                        self.new_game_dir_relative.clear();
                        self.new_exe_disk.clear();
                        self.new_exe_relative.clear();
                        self.auto_filled_dir.clear();
                        self.auto_filled_name.clear();
                        self.add_match.reset();

                        let mut tasks =
                            vec![Task::perform(async { connect_and_load().await }, |r| {
                                Message::GamesLoaded(r)
                            })];
                        if let Some((cloud_key, cloud_id, cloud_name)) = binding {
                            // 用户在本页看到的那一条:添加之后顺手认领(见 `sync.pair`)。
                            // 认领失败**不影响**上面那句"已添加"。
                            let message = self.create_msg.take().unwrap_or_default();
                            self.create_msg =
                                Some(format!("{message}\n正在与云端《{cloud_name}》绑定…"));
                            let socket = self.daemon_socket.clone();
                            tasks.push(Task::perform(
                                async move { pair_game(&socket, id, cloud_key, cloud_id).await },
                                Message::GamePaired,
                            ));
                        }
                        return Task::batch(tasks);
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            _ => unreachable!("update_add 只接添加游戏那批消息"),
        }
    }

    /// exe 落到一个真实文件上了:排一次"问云端有没有这一款"(防抖见 `model::add`)。
    ///
    /// 三个入口都会走到这里(手打、浏览、从进程挑),想漏也漏不掉。
    ///
    /// ⚠ 只有**手动添加这一条路**会问云端:扫目录批量加不联网(那一条路一次几十款,
    /// 一款读一次索引是荒唐的)。问不成也**绝不许**挡住添加。
    pub(in crate::ui) fn schedule_match(&mut self) -> Task<Message> {
        let exe = self.new_exe.trim().to_string();
        if exe.is_empty() || !std::path::Path::new(&exe).is_file() {
            // 路径还没落到一个真实文件上 —— 问也白问,顺手把上一款的结果收掉。
            self.add_match.reset();
            return Task::none();
        }
        self.add_match.typing(&exe);
        Task::perform(
            async move { tokio::time::sleep(MATCH_DEBOUNCE).await },
            move |_| Message::MatchExeReady(exe),
        )
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
        // 换了 exe，原先认出来的盘引用不再指向它 —— 清掉；随后那条路径的 infer
        // （浏览 / 挑进程 / 手打都会走到这里）会把新的填回来。
        self.new_exe_disk.clear();
        self.new_exe_relative.clear();
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

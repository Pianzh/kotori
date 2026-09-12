//! Settings page: the wine prefix and the whole cloud-sync section.

use super::*;
use iced::widget::column;

impl App {
    pub(super) fn settings_view(&self) -> Element<'_, Message> {
        let gray = Color::from_rgb8(0x9e, 0x9e, 0x9e);
        let dim = Color::from_rgb8(0x8a, 0x8a, 0x8a);

        let effective = self
            .wine_status
            .as_ref()
            .and_then(|status| status.configured.clone())
            .unwrap_or_else(|| "自动探测".to_string());
        let default_prefix = self
            .wine_status
            .as_ref()
            .map(|status| status.default_prefix.clone())
            .unwrap_or_else(|| "读取中…".to_string());
        let environment = self
            .wine_status
            .as_ref()
            .and_then(|status| status.environment.clone())
            .unwrap_or_else(|| "未设置".to_string());
        let detected: Vec<String> = self
            .wine_status
            .as_ref()
            .map(|status| status.detected.clone())
            .unwrap_or_default();

        let mut detected_list = column![].spacing(3);
        if detected.is_empty() {
            detected_list =
                detected_list.push(text("没有在常见位置发现 wine prefix").size(11).color(dim));
        } else {
            for prefix in detected {
                detected_list = detected_list.push(text(format!("· {prefix}")).size(11).color(dim));
            }
        }

        let mut body = column![
            text("设置").size(18).font(ui_font()),
            horizontal_rule(1),
            text("Wine 目录（prefix）").size(15).font(ui_font()),
            text("启动游戏时使用。留空 = 自动探测：游戏目录内的可携式 prefix → 常见位置 → ~/.wine。每个游戏也可以在详情页里单独覆盖。")
                .size(12)
                .color(gray),
            row![
                text_input("留空即自动探测", &self.wine_prefix_input)
                    .on_input(Message::WinePrefixChanged)
                    .padding([7, 10])
                    .width(Length::Fill),
                button(text("保存")).padding([8, 18]).on_press(Message::SaveWinePrefix),
                button(text("自动")).padding([8, 14]).on_press(Message::ClearWinePrefix),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            {
                let status: Element<'_, Message> = match &self.wine_msg {
                    Some(msg) => text(msg)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0xda, 0xa5))
                        .into(),
                    None => iced::widget::Space::new(0, 0).into(),
                };
                status
            },
            text(format!("当前生效：{effective}")).size(12).color(dim),
            text(format!("默认位置：{default_prefix}")).size(11).color(dim),
            text(format!("WINEPREFIX 环境变量：{environment}")).size(11).color(dim),
            text("自动探测到的 prefix：").size(11).color(dim),
            detected_list,
            horizontal_rule(1),
        ]
        .spacing(10);

        for section in self.sync_sections(gray, dim) {
            body = body.push(section);
        }

        if let Some(err) = &self.error {
            body = body.push(
                text(format!("\u{26A0} {err}"))
                    .size(12)
                    .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
            );
        }

        scrollable(body).into()
    }

    /// The cloud-sync half of the settings page.
    ///
    /// Built as separate pieces so each one can be read on its own: what state
    /// the account is in, what the settings are, the two secrets, and what has
    /// been synced.
    fn sync_sections(&self, gray: Color, dim: Color) -> Vec<Element<'_, Message>> {
        let form = &self.sync_form;
        let status = self.sync_status.as_ref();
        let ok = Color::from_rgb8(0x9e, 0xda, 0xa5);
        let warn = Color::from_rgb8(0xef, 0xc0, 0x7a);

        let mut sections = vec![
            text("云存档同步").size(15).font(ui_font()).into(),
            text(
                "存档通过 rclone 传到 Backblaze B2。默认不加密：bucket 里的存档就是普通文件，                 用任何 S3 工具都能取回，不需要 kotori，也不需要 rclone。",
            )
            .size(12)
            .color(gray)
            .into(),
        ];

        // --- state ---------------------------------------------------------
        let mut state = column![row![
            text("启用云同步").size(13),
            toggler(form.enabled)
                .on_toggle(Message::SyncToggleEnabled)
                .size(16),
            iced::widget::Space::new(Length::Fill, 0),
            button(text("测试连接"))
                .padding([6, 12])
                .on_press_maybe((!form.busy).then_some(Message::SyncTest)),
            button(text("立即同步全部"))
                .padding([6, 12])
                .on_press_maybe((!form.busy).then_some(Message::SyncNow(None))),
        ]]
        .spacing(10);

        match status {
            None => {
                state = state.push(text("读取中…").size(11).color(dim));
            }
            Some(status) => {
                state = state.push(text(format!("远端：{}", status.remote)).size(11).color(dim));
                match &status.rclone {
                    Some(path) => {
                        state = state.push(text(format!("rclone：{path}")).size(11).color(dim))
                    }
                    None => {
                        state = state.push(
                            text("rclone 未安装（Arch：sudo pacman -S rclone）")
                                .size(11)
                                .color(warn),
                        )
                    }
                }
                state = state.push(
                    text(format!("密钥环：{}", status.keyring))
                        .size(11)
                        .color(if status.ephemeral { warn } else { dim }),
                );
                if status.ephemeral {
                    state = state.push(
                        text("⚠ 本机没有运行中的系统密钥环，填进去的凭据只留在内存里，重启后要重新输入。")
                            .size(11)
                            .color(warn),
                    );
                    // The platform-specific advice lives in one place
                    // (`secrets::keyring_hint`); the UI only relays it.
                    state = state.push(text(crate::secrets::keyring_hint()).size(11).color(dim));
                }
                if let Some(problem) = &status.problem {
                    state = state.push(text(format!("待解决：{problem}")).size(11).color(warn));
                } else if status.ready {
                    state = state.push(text("✓ 已就绪").size(11).color(ok));
                }
            }
        }
        sections.push(state.into());

        // --- settings ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("连接与保留").size(13).font(ui_font()).into());
        sections.push(
            row![
                text("bucket").size(13).width(120),
                text_input("B2 上那个 bucket 的名字", &form.bucket)
                    .on_input(|v| Message::SyncField(SyncField::Bucket, v))
                    .padding([7, 10])
                    .width(Length::Fill),
                text("prefix").size(13).width(50),
                text_input("kotori", &form.prefix)
                    .on_input(|v| Message::SyncField(SyncField::Prefix, v))
                    .padding([7, 10])
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );
        sections.push(sync_input_row(
            "API endpoint",
            "留空即可（rclone 会自己找到）",
            &form.endpoint,
            SyncField::Endpoint,
            false,
        ));
        sections.push(
            text("prefix 是 bucket 里归 kotori 独占的目录，bucket 里的其他东西我们一律不碰。")
                .size(11)
                .color(dim)
                .into(),
        );
        sections.push(
            row![
                text("保留版本数").size(13).width(120),
                text_input("0 = 永久保留", &form.keep_versions)
                    .on_input(|v| Message::SyncField(SyncField::KeepVersions, v))
                    .padding([7, 10])
                    .width(120),
                text("0 表示永不删除云端快照；填写后只清理旧快照，绝不动本地存档。")
                    .size(11)
                    .color(dim),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        let mut encryption = row![
            text("加密上传（rclone crypt）").size(13),
            toggler(form.encryption)
                .on_toggle(Message::SyncEncryptionToggled)
                .size(16),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);
        if let Some(pending) = form.confirm_encryption {
            encryption = encryption.push(
                button(text(if pending {
                    "确认开启"
                } else {
                    "确认关闭"
                }))
                .padding([6, 12])
                .on_press(Message::SyncConfirmEncryption),
            );
            encryption = encryption.push(
                button(text("取消"))
                    .padding([6, 12])
                    .on_press(Message::SyncCancelEncryption),
            );
        }
        sections.push(encryption.into());
        sections.push(
            text("关闭时存档是明文文件（推荐）；开启后必须记住密码，忘了就打不开自己的备份。")
                .size(11)
                .color(dim)
                .into(),
        );

        sections.push(
            row![
                button(text("保存设置"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSaveSettings)),
                {
                    let msg: Element<'_, Message> = match &form.msg {
                        Some(msg) => text(msg.clone()).size(11).color(ok).into(),
                        None => iced::widget::Space::new(0, 0).into(),
                    };
                    msg
                },
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        // --- credentials ---------------------------------------------------
        // --- where the credentials live ------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("凭据存储").size(13).font(ui_font()).into());
        let store = status.map(|s| s.store_kind.as_str()).unwrap_or("");
        sections.push(
            text(status.map(|s| s.keyring.clone()).unwrap_or_default())
                .size(11)
                .color(dim)
                .into(),
        );

        match store {
            // A file on disk, sealed with a password only the user knows.
            "encrypted-file" => {
                if status.map(|s| s.store_locked).unwrap_or(false) {
                    sections.push(
                        text("凭据文件已锁定：输入主密码解锁（解锁后本次守护进程内一直有效）。")
                            .size(11)
                            .color(warn)
                            .into(),
                    );
                    sections.push(
                        row![
                            text("主密码").size(13).width(120),
                            text_input("凭据文件的主密码", &form.master_password)
                                .on_input(Message::SyncMasterPasswordChanged)
                                .secure(true)
                                .padding([7, 10])
                                .width(Length::Fill),
                            button(text("解锁"))
                                .padding([7, 16])
                                .on_press_maybe((!form.busy).then_some(Message::SyncUnlock)),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .into(),
                    );
                } else {
                    sections.push(text("✓ 已解锁").size(11).color(ok).into());
                }
            }
            // Nothing on this machine can persist a secret: say so, explain how
            // to fix the machine, and offer the way out that works anywhere.
            "session-only" => {
                sections.push(
                    text("⚠ 本机没有运行中的密钥环，凭据只留在内存里，重启后要重新输入。")
                        .size(11)
                        .color(warn)
                        .into(),
                );
                sections.push(
                    text(crate::secrets::keyring_hint())
                        .size(11)
                        .color(dim)
                        .into(),
                );
                sections.push(
                    text(format!(
                        "也可以在这里设一个主密码：凭据会用 Argon2id + ChaCha20-Poly1305 加密存到 {}，\
                         之后每次开机只要输一次主密码。密码由你自己保管，我们不会存它。",
                        status
                            .map(|s| s.store_path.clone())
                            .filter(|path| !path.is_empty())
                            .unwrap_or_else(|| "凭据文件".to_string())
                    ))
                    .size(11)
                    .color(dim)
                    .into(),
                );
                let hint = format!("至少 {} 位，自己记得住就行", self.min_master_password());
                sections.push(
                    row![
                        text("主密码").size(13).width(120),
                        text_input(hint.as_str(), &form.master_password)
                            .on_input(Message::SyncMasterPasswordChanged)
                            .secure(true)
                            .padding([7, 10])
                            .width(Length::Fill),
                        button(text("加密保存凭据"))
                            .padding([7, 16])
                            .on_press_maybe((!form.busy).then_some(Message::SyncSetMasterPassword)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center)
                    .into(),
                );
            }
            _ => {}
        }

        sections.push(horizontal_rule(1).into());
        sections.push(
            text("B2 凭据（只保存一套，再次保存即覆盖）")
                .size(13)
                .font(ui_font())
                .into(),
        );
        sections.push(
            text(
                "第一次用 B2 的话，先在网页控制台做两件事：\n                 1. Buckets → Create a Bucket，名字填到上面的 bucket 里（Files in Bucket 选 Private）\n                 2. Account → Application Keys → Add a New Application Key：Bucket(s) 只勾这一个 bucket，\n                 \u{20}\u{20}\u{20}Type of Access 选 Read and Write\n                 创建后会显示 keyID 和 applicationKey，只显示这一次，复制到下面两个框里。",
            )
            .size(11)
            .color(dim)
            .into(),
        );
        let known = |account: &str| status.is_some_and(|s| s.has_secret(account));
        let key_id_saved = known("b2-key-id");
        let app_key_saved = known("b2-app-key");
        let stored = usize::from(key_id_saved) + usize::from(app_key_saved);
        sections.push(
            text(credentials_label(key_id_saved, app_key_saved))
                .size(11)
                .color(if stored == 2 { ok } else { warn })
                .into(),
        );
        sections.push(sync_input_row(
            "keyID",
            "Application Key ID（形如 005a…）",
            &form.key_id,
            SyncField::KeyId,
            false,
        ));
        sections.push(sync_input_row(
            "applicationKey",
            "只在创建时显示一次，丢了就再建一个",
            &form.app_key,
            SyncField::AppKey,
            true,
        ));
        sections.push(
            row![
                button(text("保存凭据"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSaveCredentials)),
                // Deleting takes effect immediately: no need to empty the boxes
                // and save, which looked like it might do nothing.
                button(text("删除凭据")).padding([7, 16]).on_press_maybe(
                    (!form.busy && stored > 0).then_some(Message::SyncClearCredentials)
                ),
                text("两个框填的都是同一个 B2 账号，下次保存会覆盖上一套")
                    .size(11)
                    .color(dim),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into(),
        );

        // --- password ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(
            text("同步密码（仅在开启加密时使用）")
                .size(13)
                .font(ui_font())
                .into(),
        );
        sections.push(
            text(
                "密码由你自己设定，我们不会替你生成——你从没见过的密码就等于把备份锁在别人手里。                 它只进系统密钥环；忘了也能自己取回来：",
            )
            .size(11)
            .color(dim)
            .into(),
        );
        sections.push(
            text(status.map(|s| s.password_hint.clone()).unwrap_or_default())
                .size(11)
                .color(gray)
                .font(iced::Font::MONOSPACE)
                .into(),
        );
        sections.push(
            row![
                text("密码").size(13).width(120),
                text_input("留空 = 清除密码", &form.password)
                    .on_input(|v| Message::SyncField(SyncField::Password, v))
                    .secure(true)
                    .padding([7, 10])
                    .width(Length::Fill),
                text("再输一次").size(13).width(70),
                text_input("确认", &form.password_again)
                    .on_input(|v| Message::SyncField(SyncField::PasswordAgain, v))
                    .secure(true)
                    .padding([7, 10])
                    .width(Length::Fill),
                button(text("保存密码"))
                    .padding([7, 16])
                    .on_press_maybe((!form.busy).then_some(Message::SyncSavePassword)),
                button(text("删除密码")).padding([7, 16]).on_press_maybe(
                    (!form.busy && known("sync-password")).then_some(Message::SyncClearPassword)
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        );
        sections.push(
            text(format!(
                "密码状态：{}",
                if known("sync-password") {
                    "已保存在密钥环（改密码会让已加密上传的存档无法解密，会再确认一次）"
                } else {
                    "未设置（没开加密就不需要它）"
                }
            ))
            .size(11)
            .color(dim)
            .into(),
        );

        // --- per game ------------------------------------------------------
        sections.push(horizontal_rule(1).into());
        sections.push(text("各游戏存档").size(13).font(ui_font()).into());
        let games = status.map(|s| s.games.clone()).unwrap_or_default();
        if games.is_empty() {
            sections.push(text("还没有游戏").size(11).color(dim).into());
        }
        for game in games {
            let pending = self
                .sync_restore_pending
                .as_ref()
                .is_some_and(|(id, _)| *id == game.id);
            let mut line = row![
                text(game.name.clone())
                    .size(12)
                    .width(Length::FillPortion(3)),
                text(format!("{} 个位置", game.locations))
                    .size(11)
                    .color(dim)
                    .width(Length::FillPortion(1)),
                text(game.last_label())
                    .size(11)
                    .color(dim)
                    .width(Length::FillPortion(3)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);

            if game.locations > 0 && !game.problem.is_some() {
                line = line.push(button(text("同步")).padding([4, 10]).on_press_maybe(
                    (!form.busy).then_some(Message::SyncNow(Some(game.id.clone()))),
                ));
                if pending {
                    line = line.push(
                        button(text("确认恢复（会覆盖本地存档）"))
                            .padding([4, 10])
                            .on_press(Message::SyncRestoreConfirmed),
                    );
                    line = line.push(
                        button(text("取消"))
                            .padding([4, 10])
                            .on_press(Message::SyncRestoreCancelled),
                    );
                } else {
                    line = line.push(
                        button(text("恢复")).padding([4, 10]).on_press_maybe(
                            (!form.busy)
                                .then_some(Message::SyncRestoreRequested(game.id.clone(), None)),
                        ),
                    );
                }
            } else if game.locations == 0 {
                line = line.push(text("还没配置存档位置").size(11).color(dim));
            }

            sections.push(line.into());
            if let Some(problem) = &game.problem {
                sections.push(text(format!("    ⚠ {problem}")).size(11).color(warn).into());
            }
        }

        sections
    }
}

/// Label + (optionally masked) input row for the sync form.
fn sync_input_row<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
    field: SyncField,
    secret: bool,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(120),
        text_input(placeholder, value)
            .on_input(move |v| Message::SyncField(field, v))
            .secure(secret)
            .padding([7, 10])
            .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

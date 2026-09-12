//! One game's detail page: scale profile, paths and save locations.

use super::*;
use iced::widget::column;

impl App {
    pub(super) fn game_detail_view(&self) -> Element<'_, Message> {
        let Some(draft) = &self.draft else {
            return self.games_view();
        };

        let back_btn = button(text("← 返回")).on_press(Message::BackToList);

        let algo_options: Vec<String> = ScaleAlgorithm::ALL.iter().map(|s| s.to_string()).collect();
        let algo_pick: iced::widget::PickList<
            '_,
            String,
            Vec<String>,
            String,
            Message,
            Theme,
            iced::Renderer,
        > = pick_list(algo_options, Some(draft.algo.clone()), |algo| {
            Message::AlgoChanged(algo)
        });

        let show_sharpness = matches!(draft.algo.as_str(), "Fsr" | "Nis");
        let sharpness_row: Element<'_, Message> = if show_sharpness {
            let sharp: iced::widget::Slider<'_, f32, Message> =
                slider(0.0..=5.0, draft.sharpness as f32, Message::SharpnessChanged);
            row![
                text("锐度").size(13).width(80),
                sharp.width(200),
                text(format!("{}", draft.sharpness))
                    .size(13)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into()
        } else {
            iced::widget::row![].into()
        };

        let num_input = |label: &'static str,
                         value: String,
                         msg: fn(String) -> Message|
         -> Element<'_, Message> {
            row![
                text(label).size(13).width(120),
                text_input("", &value)
                    .on_input(msg)
                    .padding([6, 8])
                    .width(120),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(8)
            .into()
        };

        let fullscreen_toggle: Element<'_, Message> = row![
            text("全屏启动").size(13).width(120),
            toggler(draft.fullscreen).on_toggle(Message::FullscreenToggled),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let framerate_input: Element<'_, Message> = row![
            text("帧率限制 (留空不限)").size(13).width(120),
            text_input("60", &draft.framerate)
                .on_input(Message::FramerateChanged)
                .padding([6, 8])
                .width(120),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let game_dir_row: Element<'_, Message> = row![
            text("游戏根目录").size(13).width(120),
            text_input("/path/to/game", &draft.game_dir)
                .on_input(Message::GameDirChanged)
                .padding([6, 8])
                .width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let exe_row: Element<'_, Message> = row![
            text("可执行文件").size(13).width(120),
            text_input("/path/to/game.exe", &draft.exe)
                .on_input(Message::ExePathChanged)
                .padding([6, 8])
                .width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .into();

        let save_btn = button(text(if self.saving {
            "保存中..."
        } else {
            "保存配置"
        }))
        .padding([10, 24])
        .on_press(Message::SaveProfile);

        let algo_row: Element<'_, Message> = iced::widget::Row::new()
            .push(text("缩放算法").size(13).width(120))
            .push(algo_pick)
            .align_y(iced::Alignment::Center)
            .spacing(8)
            .into();

        // Save locations: three kinds, each optionally with exclude patterns.
        let mut save_rows = column![].spacing(6);
        for (index, entry) in draft.save_paths.iter().enumerate() {
            let kind_pick = pick_list(
                SAVE_PATH_KINDS.map(str::to_string).to_vec(),
                Some(entry.kind.clone()),
                move |kind| Message::SavePathKindChanged(index, kind),
            );
            save_rows = save_rows.push(
                row![
                    kind_pick,
                    text_input(kind_placeholder(&entry.kind), &entry.path)
                        .on_input(move |value| Message::SavePathChanged(index, value))
                        .padding([6, 8])
                        .width(Length::Fill),
                    text_input("排除：*.log, cache/", &entry.exclude)
                        .on_input(move |value| Message::SavePathExcludeChanged(index, value))
                        .padding([6, 8])
                        .width(190),
                    button(text("删除").size(11))
                        .padding([6, 10])
                        .on_press(Message::RemoveSavePath(index)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }

        let mut body = column![
            row![back_btn, iced::widget::horizontal_space()],
            text(&draft.game_name)
                .size(20)
                .font(ui_font()),
            text("每次修改保存后，重新启动游戏即生效。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            horizontal_rule(1),
            game_dir_row,
            exe_row,
            algo_row,
            sharpness_row,
            num_input("游戏分辨率宽", draft.internal_w.clone(), Message::InternalWChanged),
            num_input("游戏分辨率高", draft.internal_h.clone(), Message::InternalHChanged),
            num_input("输出分辨率宽", draft.output_w.clone(), Message::OutputWChanged),
            num_input("输出分辨率高", draft.output_h.clone(), Message::OutputHChanged),
            num_input("缩放倍数", draft.scale_ratio.clone(), Message::ScaleRatioChanged),
            text("缩放倍数 = 输出像素 ÷ 游戏自身分辨率，填了它就以它为准（输出分辨率宽/高只作为参考显示）。启动游戏时按这个倍数开窗，快捷键「按设定比例缩放／取消缩放」（默认 Shift+Alt+Q）也是在这个倍数和 1:1 之间来回切。留空则沿用输出分辨率。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            text("提示：在 Niri 等平铺桌面下游戏会铺满整块显示器，输出分辨率主要影响缩放计算；窗口缩放在 KDE 上通过 KWin 完成。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            fullscreen_toggle,
            framerate_input,
            horizontal_rule(1),
            text("存档位置").size(15).font(ui_font()),
            text("windows = prefix 内的 Windows 路径，推荐用 %APPDATA% / %DOCUMENTS% / %SAVEDGAMES% 令牌（不要写 C:\\users\\<用户名>，各 prefix 的用户名不一样）；relative = 相对游戏根目录；absolute = 仅本机，不跨平台同步。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            save_rows,
            button(text("添加存档位置").size(12))
                .padding([6, 12])
                .on_press(Message::AddSavePath),
            horizontal_rule(1),
            {
                let save_status: Element<'_, Message> = match &self.saved_msg {
                    Some(msg) => {
                        text(msg).size(12).color(Color::from_rgb8(0x9e, 0xda, 0xa5)).into()
                    }
                    None => iced::widget::Space::new(0, 0).into(),
                };
                row![save_btn, save_status]
                    .align_y(iced::Alignment::Center)
                    .spacing(12)
            },
            text("提示：游戏窗口聚焦时，可用 gamescope 快捷键实时切换：Super+U FSR、Super+Y NIS、Super+N 最近邻、Super+I/O 锐度增/减。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            horizontal_rule(1),
            {
                // Deleting only drops the library entry, never the game files.
                let delete_area: Element<'_, Message> = if self.confirm_delete {
                    row![
                        text("删除这个条目？（不会删除游戏文件）")
                            .size(12)
                            .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
                        button(text("确认删除"))
                            .padding([6, 14])
                            .on_press(Message::DeleteConfirmed),
                        button(text("取消"))
                            .padding([6, 14])
                            .on_press(Message::DeleteCancelled),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
                } else {
                    button(text("删除条目"))
                        .padding([6, 14])
                        .on_press(Message::DeleteRequested)
                        .into()
                };
                delete_area
            },
        ]
        .spacing(10);

        if self.games.is_empty() {
            body = body.push(
                text("等待加载...")
                    .size(12)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        }

        scrollable(body).into()
    }
}

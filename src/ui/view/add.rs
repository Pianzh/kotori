//! The "add game" page (manual entry) and its input row helper.

use super::*;
use iced::widget::column;

impl App {
    /// Manual add: name + game root + executable. No scanning, no guessing.
    pub(super) fn add_view(&self) -> Element<'_, Message> {
        let create_btn = button(text(if self.creating {
            "添加中…"
        } else {
            "添加游戏"
        }))
        .padding([8, 20])
        .on_press(Message::CreateRequested);

        let mut body = column![
            text("添加游戏").size(18).font(ui_font()),
            horizontal_rule(1),
            text("手动填写。游戏根目录是启动时的工作目录，也是存档相对路径的基准；留空则取可执行文件所在目录。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            labeled_input("游戏名", "例如 3days", &self.new_name, Message::NewNameChanged),
            labeled_input(
                "游戏根目录",
                "/path/to/game",
                &self.new_game_dir,
                Message::NewGameDirChanged,
            ),
            labeled_input(
                "可执行文件",
                "/path/to/game.exe",
                &self.new_exe,
                Message::NewExeChanged,
            ),
            {
                let status: Element<'_, Message> = match &self.create_msg {
                    Some(msg) => text(msg)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0xda, 0xa5))
                        .into(),
                    None => iced::widget::Space::new(0, 0).into(),
                };
                row![create_btn, status]
                    .spacing(12)
                    .align_y(iced::Alignment::Center)
            },
            text("添加后可在游戏库的详情页里继续配置缩放、Wine 目录与存档位置。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
        ]
        .spacing(10);

        if let Some(err) = &self.error {
            body = body.push(
                text(format!("\u{26A0} {err}"))
                    .size(12)
                    .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
            );
        }

        scrollable(body).into()
    }
}

/// Label + text input row used by the add form.
fn labeled_input<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
    on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    row![
        text(label).size(13).width(100),
        text_input(placeholder, value)
            .on_input(on_input)
            .padding([7, 10])
            .width(Length::Fill),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(8)
    .into()
}

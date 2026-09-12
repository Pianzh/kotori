//! Library list: the game cards and the search filter.

use super::*;
use iced::widget::column;

impl App {
    pub(super) fn games_view(&self) -> Element<'_, Message> {
        let visible: Vec<&UiGame> = self
            .games
            .iter()
            .filter(|g| matches_query(g, &self.search))
            .collect();

        let count = if self.search.trim().is_empty() {
            format!("游戏库 ({})", self.games.len())
        } else {
            format!("游戏库 ({} / {})", visible.len(), self.games.len())
        };
        let refresh_btn = button(text(if self.loading {
            "加载中..."
        } else {
            "刷新"
        }))
        .on_press(Message::Refresh);

        let search_input = text_input("搜索游戏名或路径…", &self.search)
            .on_input(Message::SearchChanged)
            .padding([7, 10]);

        let mut list = column![
            row![
                text(count).size(18).font(ui_font()),
                iced::widget::horizontal_space(),
                refresh_btn,
            ]
            .align_y(iced::Alignment::Center),
            search_input,
        ]
        .spacing(8);

        if let Some(err) = &self.error {
            list = list.push(
                container(
                    text(format!("\u{26A0} {err}"))
                        .size(13)
                        .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
                )
                .padding(10)
                .width(Length::Fill)
                .style(|_theme: &Theme| iced::widget::container::Style {
                    background: Some(Color::from_rgb8(0x3a, 0x22, 0x22).into()),
                    border: iced::border::Border::default().rounded(6),
                    ..Default::default()
                }),
            );
        }

        if self.games.is_empty() && self.error.is_none() {
            list = list.push(
                text(if self.loading {
                    "正在从守护进程加载游戏列表..."
                } else {
                    "还没有游戏。切到「添加游戏」扫描一个目录，或执行 `kotori scan <目录>`。"
                })
                .size(13)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        } else if visible.is_empty() {
            list = list.push(
                text(format!("没有匹配「{}」的游戏", self.search.trim()))
                    .size(13)
                    .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            );
        }

        if self.games.is_empty() {
            list.into()
        } else {
            scrollable(
                list.push(
                    column(
                        visible
                            .iter()
                            .map(|g| self.game_card(g))
                            .collect::<Vec<_>>(),
                    )
                    .spacing(8),
                ),
            )
            .into()
        }
    }

    fn game_card(&self, game: &UiGame) -> Element<'_, Message> {
        let launching = self.launching.as_deref() == Some(game.id.as_str());
        let session = self.running.get(&game.id);

        // A live session turns the action button into "stop"; a watch-only game
        // that is not running yet offers "monitor" instead of "launch".
        let action_btn = if session.is_some() {
            button(text("停止"))
                .padding([8, 18])
                .on_press(Message::Stop(game.id.clone()))
                .style(
                    |_t: &Theme, _s: iced::widget::button::Status| iced::widget::button::Style {
                        background: Some(Color::from_rgb8(0x8c, 0x3b, 0x3b).into()),
                        text_color: Color::WHITE,
                        ..Default::default()
                    },
                )
        } else {
            let label = if launching {
                "启动中…"
            } else if game.watch_only {
                "监视"
            } else {
                "启动"
            };
            button(text(label))
                .padding([8, 18])
                .on_press(Message::Launch(game.id.clone()))
                .style(move |_t: &Theme, _s: iced::widget::button::Status| {
                    iced::widget::button::Style {
                        background: Some(
                            if launching {
                                Color::from_rgb8(0x37, 0x40, 0x51)
                            } else {
                                Color::from_rgb8(0x2c, 0x6b, 0xbf)
                            }
                            .into(),
                        ),
                        text_color: Color::WHITE,
                        ..Default::default()
                    }
                })
        };

        let status_badge: Element<'_, Message> = match session {
            Some(session) if session.watch_only => text("● 监视中")
                .size(11)
                .color(Color::from_rgb8(0xd8, 0xa6, 0x57))
                .into(),
            Some(_) => text("● 运行中")
                .size(11)
                .color(Color::from_rgb8(0x4c, 0xaf, 0x50))
                .into(),
            None => iced::widget::Space::new(0, 0).into(),
        };

        let edit_btn = button(text("配置"))
            .padding([8, 14])
            .on_press(Message::GameSelected(game.id.clone()));

        let mut details = column![
            text(game.name.clone()).size(15).font(ui_font()),
            text(game.exe.clone())
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            text(game.scale_label())
                .size(11)
                .color(Color::from_rgb8(0x7a, 0xaa, 0x7f)),
        ]
        .spacing(3)
        .align_x(iced::Alignment::Start);

        if game.watch_only {
            details = details.push(
                text(format!(
                    "仅观测：由你自行启动，kotori 跟随进程 {}",
                    if game.process_name.is_empty() {
                        "（未设置）"
                    } else {
                        &game.process_name
                    }
                ))
                .size(11)
                .color(Color::from_rgb8(0xd8, 0xa6, 0x57)),
            );
        }

        container(
            row![
                details,
                iced::widget::horizontal_space(),
                status_badge,
                edit_btn,
                action_btn,
            ]
            .align_y(iced::Alignment::Center)
            .padding([14, 14])
            .spacing(8),
        )
        .width(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(Color::from_rgb8(0x22, 0x27, 0x2e).into()),
            border: iced::border::Border::default().rounded(8),
            ..Default::default()
        })
        .into()
    }
}

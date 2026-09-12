//! The widget tree. `view` dispatches to one page per tab; the pages are the
//! sibling modules below.

mod add;
mod detail;
mod games;
mod settings;

use super::*;
use iced::widget::column;

impl App {
    pub fn view(&self) -> Element<'_, Message> {
        let content = match self.tab {
            Tab::Games => {
                if self.selected.is_some() {
                    self.game_detail_view()
                } else {
                    self.games_view()
                }
            }
            Tab::Settings => self.settings_view(),
            Tab::Add => self.add_view(),
        };

        row![
            self.sidebar(),
            container(content)
                .padding(18)
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .height(Length::Fill)
        .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let daemon_status = match self.daemon_connected {
            Some(true) => ("已连接", Color::from_rgb8(0x4c, 0xaf, 0x50)),
            Some(false) if self.retry_attempts <= MAX_AUTO_RETRIES && self.retry_attempts > 0 => {
                ("未连接（重试中…）", Color::from_rgb8(0xe5, 0x39, 0x35))
            }
            Some(false) => ("未连接", Color::from_rgb8(0xe5, 0x39, 0x35)),
            None => ("检测中...", Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        };

        let mut status_row = row![
            text("\u{25CF}").size(12).color(daemon_status.1),
            text(daemon_status.0)
                .size(11)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        if self.daemon_connected == Some(false) {
            status_row = status_row.push(
                button(text("重连").size(11))
                    .padding([3, 8])
                    .on_press(Message::Refresh),
            );
        }

        container(
            column![
                text("Kotori").size(20).font(ui_font()),
                horizontal_rule(1),
                self.nav_item(Tab::Games, "游戏库"),
                self.nav_item(Tab::Add, "添加游戏"),
                self.nav_item(Tab::Settings, "设置"),
                iced::widget::Space::with_height(Length::Fill),
                status_row,
            ]
            .spacing(4)
            .padding(16),
        )
        .width(200)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(Color::from_rgb8(0x1b, 0x1e, 0x24).into()),
            ..Default::default()
        })
        .into()
    }

    fn nav_item(&self, tab: Tab, label: &'static str) -> Element<'_, Message> {
        let active = self.tab == tab;
        button(text(label).size(15))
            .padding([10, 14])
            .width(Length::Fill)
            .on_press(Message::TabChanged(tab))
            .style(move |_t: &Theme, _s: iced::widget::button::Status| {
                iced::widget::button::Style {
                    background: Some(
                        if active {
                            Color::from_rgb8(0x2c, 0x6b, 0xbf)
                        } else {
                            Color::TRANSPARENT
                        }
                        .into(),
                    ),
                    text_color: if active {
                        Color::WHITE
                    } else {
                        Color::from_rgb8(0xc8, 0xc8, 0xc8)
                    },
                    ..Default::default()
                }
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture, ui_game};

    #[test]
    fn views_construct_for_every_tab_and_state() {
        let (mut app, _task) = App::new();

        // Library: empty, populated, filtered, no match.
        app.tab = Tab::Games;
        let _ = app.view();
        app.games = vec![ui_game()];
        let _ = app.view();

        // A watch-only card, with and without a process name.
        app.games = vec![UiGame {
            watch_only: true,
            process_name: "game.exe".into(),
            ..ui_game()
        }];
        let _ = app.view();
        app.games = vec![ui_game()];

        // Live sessions (running vs. monitoring) change the action button.
        app.running.insert(
            "demo".into(),
            SessionInfo {
                session_id: "session-1".into(),
                watch_only: false,
            },
        );
        let _ = app.view();
        app.running.insert(
            "demo".into(),
            SessionInfo {
                session_id: "session-1".into(),
                watch_only: true,
            },
        );
        let _ = app.view();
        app.running.clear();

        app.search = "demo".into();
        let _ = app.view();
        app.search = "zzz".into();
        let _ = app.view();
        app.search.clear();

        // Detail page, with and without the delete confirmation.
        app.selected = Some("demo".into());
        app.draft = Some(Draft::from_game(&ui_game()));
        let _ = app.view();

        // With save locations in the editor (one of each kind).
        app.draft = Some(Draft {
            save_paths: vec![
                SavePathDraft {
                    kind: "windows".into(),
                    path: "%APPDATA%\\Game".into(),
                    exclude: "*.log".into(),
                },
                SavePathDraft {
                    kind: "relative".into(),
                    path: "savedata".into(),
                    exclude: String::new(),
                },
                SavePathDraft {
                    kind: "absolute".into(),
                    path: "/saves/demo".into(),
                    exclude: String::new(),
                },
            ],
            ..Draft::from_game(&ui_game())
        });
        let _ = app.view();

        app.confirm_delete = true;
        let _ = app.view();

        // Add page: empty form, filled form, with and without a message.
        app.tab = Tab::Add;
        app.selected = None;
        app.draft = None;
        app.confirm_delete = false;
        let _ = app.view();
        app.new_name = "Demo".into();
        app.new_game_dir = "/games/demo".into();
        app.new_exe = "/games/demo/game.exe".into();
        let _ = app.view();
        app.create_msg = Some("已添加（ID: demo）".into());
        let _ = app.view();

        // Settings page: before and after the wine status arrives.
        app.tab = Tab::Settings;
        app.wine_status = None;
        let _ = app.view();
        app.wine_status = Some(WineStatus {
            configured: Some("/prefixes/games".into()),
            default_prefix: "/home/user/.wine".into(),
            environment: None,
            detected: vec!["/home/user/.wine".into()],
        });
        app.wine_msg = Some("已保存".into());
        let _ = app.view();

        // Sync section: nothing loaded yet, then a ready setup, then the two
        // states that ask for a decision (a restore and an encryption flip).
        app.sync_status = None;
        app.sync_form = SyncForm::default();
        let _ = app.view();

        app.sync_status = Some(sync_status_fixture());
        app.sync_form.apply(
            app.sync_status.as_ref().unwrap(),
            &sync_payload()["settings"],
        );
        let _ = app.view();

        app.sync_form.confirm_encryption = Some(true);
        let _ = app.view();
        app.sync_form.confirm_encryption = None;

        app.sync_restore_pending = Some(("demo".into(), None));
        let _ = app.view();
        app.sync_restore_pending = None;

        // A locked credential file: the page must offer "unlock", not
        // "enter your B2 keys again".
        app.sync_status = Some(SyncStatus {
            store_kind: "encrypted-file".into(),
            store_locked: true,
            store_path: "/home/user/.config/kotori/secrets.json".into(),
            keyring: "主密码加密文件 /home/user/.config/kotori/secrets.json（已锁定）".into(),
            secrets: Vec::new(),
            ready: false,
            problem: Some("凭据文件已锁定，请先用主密码解锁".into()),
            ..sync_status_fixture()
        });
        app.sync_form.master_password = "typed".into();
        let _ = app.view();

        // ...and once unlocked, no password field at all.
        app.sync_status = Some(SyncStatus {
            store_locked: false,
            secrets: vec!["b2-key-id".into(), "b2-app-key".into()],
            ready: true,
            problem: None,
            ..app.sync_status.clone().unwrap()
        });
        let _ = app.view();

        // A machine with no keyring at all: explain, and offer the way out.
        app.sync_status = Some(SyncStatus {
            store_kind: "session-only".into(),
            store_locked: false,
            keyring: "内存（本机没有运行中的系统密钥环，重启后需要重新输入）".into(),
            ephemeral: true,
            ..sync_status_fixture()
        });
        let _ = app.view();
        app.sync_status = Some(sync_status_fixture());
        app.sync_form.master_password.clear();

        // A machine with no keyring, no rclone and an unresolvable save path.
        app.sync_status = Some(SyncStatus {
            rclone: None,
            ephemeral: true,
            keyring: "内存（没有系统密钥环，重启后需重新输入）".into(),
            problem: Some("密钥环里还没有 B2 凭据".into()),
            games: vec![SyncGameRow {
                id: "demo".into(),
                name: "Demo".into(),
                locations: 0,
                problem: Some("存档位置「%NOPE%」解析不了".into()),
                last: Some("✗ 2026-09-11T10:15 ✓".into()),
            }],
            ..sync_status_fixture()
        });
        let _ = app.view();

        // Settings, plus the disconnected sidebar with its reconnect button.
        app.tab = Tab::Settings;
        for (connected, attempts) in [
            (Some(true), 0),
            (Some(false), 1),
            (Some(false), 99),
            (None, 0),
        ] {
            app.daemon_connected = connected;
            app.retry_attempts = attempts;
            app.error = Some("boom".into());
            let _ = app.view();
        }
    }
}

use std::path::{Path, PathBuf};

use iced::widget::{
    button, column, container, horizontal_rule, pick_list, row, scrollable, slider, text,
    text_input, toggler,
};
use iced::{Color, Element, Length, Size, Task, Theme};
use serde_json::Value;

use crate::config::{ScaleAlgorithm, ScaleProfile};

const UI_FONT_FAMILY: &str = "Kotori Sans";
const UI_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/KotoriSans-Regular.ttf");

fn ui_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(UI_FONT_FAMILY),
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Games,
    Add,
    Settings,
}

/// A game found by a directory scan (preview only, not added yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCandidate {
    pub id: String,
    pub name: String,
    pub exe: String,
    pub is_new: bool,
}

/// Maximum number of automatic reconnect attempts before giving up (a manual
/// "重连" always works, and resets the counter).
const MAX_AUTO_RETRIES: u32 = 5;

#[derive(Debug, Clone)]
pub struct UiGame {
    pub id: String,
    pub name: String,
    pub exe: String,
    /// Profile name as stored, so saving never silently renames it.
    pub profile_name: String,
    pub algo: String,
    pub sharpness: u32,
    pub internal: (u32, u32),
    pub output: (u32, u32),
    pub fullscreen: bool,
    pub framerate: Option<u32>,
}

impl UiGame {
    fn scale_label(&self) -> String {
        format!(
            "{}  {}x{} -> {}x{}",
            self.algo, self.internal.0, self.internal.1, self.output.0, self.output.1
        )
    }
}

/// Editable copy of a game's scale profile.
#[derive(Debug, Clone)]
struct Draft {
    game_id: String,
    game_name: String,
    profile_name: String,
    /// Editable exe path, plus the stored value so an unchanged path is not
    /// re-sent (the daemon rejects a path whose file is missing, e.g. when the
    /// game lives on a drive that is not mounted right now).
    exe: String,
    exe_original: String,
    algo: String,
    sharpness: u32,
    internal_w: String,
    internal_h: String,
    output_w: String,
    output_h: String,
    fullscreen: bool,
    framerate: String,
}

impl Draft {
    /// Seed the form from the *stored* profile. Anything else means a plain
    /// "open + save" silently rewrites the user's settings.
    fn from_game(game: &UiGame) -> Self {
        Self {
            game_id: game.id.clone(),
            game_name: game.name.clone(),
            profile_name: game.profile_name.clone(),
            exe: game.exe.clone(),
            exe_original: game.exe.clone(),
            algo: if ScaleAlgorithm::ALL.contains(&game.algo.as_str()) {
                game.algo.clone()
            } else {
                ScaleAlgorithm::Fsr {
                    sharpness: game.sharpness,
                }
                .label()
                .to_string()
            },
            sharpness: game.sharpness,
            internal_w: game.internal.0.to_string(),
            internal_h: game.internal.1.to_string(),
            output_w: game.output.0.to_string(),
            output_h: game.output.1.to_string(),
            fullscreen: game.fullscreen,
            framerate: game.framerate.map(|f| f.to_string()).unwrap_or_default(),
        }
    }

    /// Has the user changed the exe path?
    fn exe_changed(&self) -> bool {
        self.exe.trim() != self.exe_original
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TabChanged(Tab),
    Refresh,
    GamesLoaded(Result<Vec<UiGame>, String>),
    Launch(String),
    LaunchDone(Result<Value, String>),
    GameSelected(String),
    BackToList,
    SearchChanged(String),
    AlgoChanged(String),
    SharpnessChanged(f32),
    InternalWChanged(String),
    InternalHChanged(String),
    OutputWChanged(String),
    OutputHChanged(String),
    FullscreenToggled(bool),
    FramerateChanged(String),
    ExePathChanged(String),
    SaveProfile,
    ProfileSaved(Result<(), String>),
    DeleteRequested,
    DeleteCancelled,
    DeleteConfirmed,
    Deleted(Result<(), String>),
    ScanDirChanged(String),
    ScanRequested,
    ScanFinished(Result<Vec<ScanCandidate>, String>),
    AddRequested,
    AddFinished(Result<String, String>),
}

pub struct App {
    tab: Tab,
    games: Vec<UiGame>,
    daemon_socket: PathBuf,
    daemon_connected: Option<bool>,
    loading: bool,
    error: Option<String>,
    launching: Option<String>,
    selected: Option<String>,
    draft: Option<Draft>,
    saving: bool,
    saved_msg: Option<String>,
    /// Library search query (matches name or exe path).
    search: String,
    confirm_delete: bool,
    /// "Add games" tab state.
    add_dir: String,
    scan_results: Option<Vec<ScanCandidate>>,
    scanning: bool,
    adding: bool,
    add_msg: Option<String>,
    /// Automatic reconnect bookkeeping.
    retry_attempts: u32,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let socket = crate::config::socket_path();

        (
            Self {
                tab: Tab::Games,
                games: Vec::new(),
                daemon_socket: socket,
                daemon_connected: None,
                loading: false,
                error: None,
                launching: None,
                selected: None,
                draft: None,
                saving: false,
                saved_msg: None,
                search: String::new(),
                confirm_delete: false,
                add_dir: String::new(),
                scan_results: None,
                scanning: false,
                adding: false,
                add_msg: None,
                retry_attempts: 0,
            },
            Task::perform(async { connect_and_load().await }, Message::GamesLoaded),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabChanged(tab) => {
                self.tab = tab;
                self.selected = None;
                self.draft = None;
                self.confirm_delete = false;
                self.error = None;
                Task::none()
            }
            Message::Refresh => {
                self.error = None;
                self.loading = true;
                Task::perform(async { connect_and_load().await }, Message::GamesLoaded)
            }
            Message::GamesLoaded(Ok(games)) => {
                self.games = games;
                self.loading = false;
                self.daemon_connected = Some(true);
                self.error = None;
                self.retry_attempts = 0;
                Task::none()
            }
            Message::GamesLoaded(Err(e)) => {
                self.loading = false;
                self.daemon_connected = Some(false);
                self.error = Some(e);
                // Self-heal: keep retrying with backoff, so the UI recovers on
                // its own once the daemon is back.
                self.retry_attempts = self.retry_attempts.saturating_add(1);
                if self.retry_attempts <= MAX_AUTO_RETRIES {
                    let delay = retry_delay(self.retry_attempts);
                    return Task::perform(async move { tokio::time::sleep(delay).await }, |_| {
                        Message::Refresh
                    });
                }
                Task::none()
            }
            Message::Launch(id) => {
                self.launching = Some(id.clone());
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move {
                        let mut params = serde_json::Map::new();
                        params.insert("id".into(), Value::String(id));
                        crate::rpc::call(&socket, "game.launch", Some(params)).await
                    },
                    Message::LaunchDone,
                )
            }
            Message::LaunchDone(result) => {
                self.launching = None;
                match result {
                    Ok(value) => {
                        let sid = value
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        tracing::info!("game launched, session={sid}");
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::GameSelected(id) => {
                if let Some(g) = self.games.iter().find(|g| g.id == id) {
                    self.selected = Some(g.id.clone());
                    self.saved_msg = None;
                    self.confirm_delete = false;
                    // Seed the form from the *stored* profile. Anything else
                    // means a plain "open + save" silently rewrites settings.
                    self.draft = Some(Draft::from_game(g));
                }
                Task::none()
            }
            Message::BackToList => {
                self.selected = None;
                self.draft = None;
                self.saved_msg = None;
                self.confirm_delete = false;
                Task::none()
            }
            Message::SearchChanged(query) => {
                self.search = query;
                Task::none()
            }
            Message::AlgoChanged(algo) => {
                if let Some(d) = &mut self.draft {
                    d.algo = algo;
                }
                Task::none()
            }
            Message::SharpnessChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.sharpness = v.round() as u32;
                }
                Task::none()
            }
            Message::InternalWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_w = v;
                }
                Task::none()
            }
            Message::InternalHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.internal_h = v;
                }
                Task::none()
            }
            Message::OutputWChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_w = v;
                }
                Task::none()
            }
            Message::OutputHChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.output_h = v;
                }
                Task::none()
            }
            Message::FullscreenToggled(b) => {
                if let Some(d) = &mut self.draft {
                    d.fullscreen = b;
                }
                Task::none()
            }
            Message::FramerateChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.framerate = v;
                }
                Task::none()
            }
            Message::ExePathChanged(v) => {
                if let Some(d) = &mut self.draft {
                    d.exe = v;
                }
                Task::none()
            }
            Message::DeleteRequested => {
                self.confirm_delete = true;
                Task::none()
            }
            Message::DeleteCancelled => {
                self.confirm_delete = false;
                Task::none()
            }
            Message::DeleteConfirmed => {
                let Some(game_id) = self.selected.clone() else {
                    return Task::none();
                };
                self.confirm_delete = false;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { remove_game(&socket, &game_id).await },
                    Message::Deleted,
                )
            }
            Message::Deleted(result) => {
                match result {
                    Ok(()) => {
                        self.selected = None;
                        self.draft = None;
                        self.error = None;
                        return Task::perform(async { connect_and_load().await }, |r| {
                            Message::GamesLoaded(r)
                        });
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::ScanDirChanged(dir) => {
                self.add_dir = dir;
                Task::none()
            }
            Message::ScanRequested => {
                let dir = self.add_dir.trim().to_string();
                if dir.is_empty() {
                    self.add_msg = Some("请先填写要扫描的目录".to_string());
                    return Task::none();
                }
                self.scanning = true;
                self.add_msg = None;
                self.error = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { scan_directory(&socket, &dir).await },
                    Message::ScanFinished,
                )
            }
            Message::ScanFinished(result) => {
                self.scanning = false;
                match result {
                    Ok(found) => {
                        self.add_msg = Some(format!(
                            "发现 {} 个游戏目录（其中 {} 个尚未添加）",
                            found.len(),
                            found.iter().filter(|c| c.is_new).count()
                        ));
                        self.scan_results = Some(found);
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::AddRequested => {
                let dir = self.add_dir.trim().to_string();
                if dir.is_empty() {
                    self.add_msg = Some("请先填写要扫描的目录".to_string());
                    return Task::none();
                }
                self.adding = true;
                self.add_msg = None;
                self.error = None;
                let socket = self.daemon_socket.clone();
                Task::perform(
                    async move { add_directory(&socket, &dir).await },
                    Message::AddFinished,
                )
            }
            Message::AddFinished(result) => {
                self.adding = false;
                match result {
                    Ok(msg) => {
                        self.add_msg = Some(msg);
                        // Re-scan so the "new" badges and the library refresh.
                        let dir = self.add_dir.trim().to_string();
                        let socket = self.daemon_socket.clone();
                        return Task::batch([
                            Task::perform(
                                async move { scan_directory(&socket, &dir).await },
                                Message::ScanFinished,
                            ),
                            Task::perform(async { connect_and_load().await }, Message::GamesLoaded),
                        ]);
                    }
                    Err(e) => self.error = Some(e),
                }
                Task::none()
            }
            Message::SaveProfile => {
                let Some(draft) = self.draft.clone() else {
                    return Task::none();
                };
                self.saving = true;
                self.saved_msg = None;
                Task::perform(
                    async move { save_profile(draft).await },
                    Message::ProfileSaved,
                )
            }
            Message::ProfileSaved(result) => {
                self.saving = false;
                self.saved_msg = Some(match &result {
                    Ok(()) => "已保存并通知守护进程".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                });
                if let Err(e) = &result {
                    self.error = Some(e.clone());
                } else {
                    // Refresh the library so the new scale shows up.
                    return Task::perform(async { connect_and_load().await }, |r| {
                        Message::GamesLoaded(r)
                    });
                }
                Task::none()
            }
        }
    }

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

    fn games_view(&self) -> Element<'_, Message> {
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

    /// Directory scan / add page.
    fn add_view(&self) -> Element<'_, Message> {
        let dir_input = text_input("例如 /run/media/<盘>/BTL", &self.add_dir)
            .on_input(Message::ScanDirChanged)
            .on_submit(Message::ScanRequested)
            .padding([8, 10])
            .width(Length::Fill);

        let scan_btn = button(text(if self.scanning {
            "扫描中…"
        } else {
            "扫描"
        }))
        .padding([8, 18])
        .on_press(Message::ScanRequested);

        let add_btn = button(text(if self.adding {
            "添加中…"
        } else {
            "添加新游戏"
        }))
        .padding([8, 18])
        .on_press(Message::AddRequested);

        let mut body = column![
            text("添加游戏").size(18).font(ui_font()),
            horizontal_rule(1),
            text("填写「装着多款游戏子目录」的父目录（例如外置盘上的 BTL），先扫描预览，再一次性添加。已存在的游戏不会被覆盖。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            row![dir_input, scan_btn]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            {
                let status: Element<'_, Message> = match &self.add_msg {
                    Some(msg) => text(msg)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0xda, 0xa5))
                        .into(),
                    None => iced::widget::Space::new(0, 0).into(),
                };
                row![add_btn, status]
                    .spacing(12)
                    .align_y(iced::Alignment::Center)
            },
        ]
        .spacing(10);

        if let Some(err) = &self.error {
            body = body.push(
                text(format!("\u{26A0} {err}"))
                    .size(12)
                    .color(Color::from_rgb8(0xef, 0x9a, 0x9a)),
            );
        }

        match &self.scan_results {
            None => body.into(),
            Some(found) if found.is_empty() => body
                .push(
                    text("该目录下没有发现可识别的游戏（需要「子目录里含 .exe」的结构）。")
                        .size(13)
                        .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
                )
                .into(),
            Some(found) => scrollable(
                body.push(
                    column(
                        found
                            .iter()
                            .map(|c| self.candidate_row(c))
                            .collect::<Vec<_>>(),
                    )
                    .spacing(6),
                ),
            )
            .into(),
        }
    }

    fn candidate_row(&self, candidate: &ScanCandidate) -> Element<'_, Message> {
        let badge = if candidate.is_new {
            text("新")
                .size(11)
                .color(Color::from_rgb8(0x7a, 0xaa, 0x7f))
        } else {
            text("已存在")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a))
        };

        container(
            row![
                column![
                    text(candidate.name.clone()).size(14).font(ui_font()),
                    text(candidate.exe.clone())
                        .size(11)
                        .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
                ]
                .spacing(3)
                .align_x(iced::Alignment::Start),
                iced::widget::horizontal_space(),
                badge,
            ]
            .align_y(iced::Alignment::Center)
            .padding([10, 12])
            .spacing(8),
        )
        .width(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(Color::from_rgb8(0x22, 0x27, 0x2e).into()),
            border: iced::border::Border::default().rounded(6),
            ..Default::default()
        })
        .into()
    }

    fn game_card(&self, game: &UiGame) -> Element<'_, Message> {
        let launching = self.launching.as_deref() == Some(game.id.as_str());
        let launch_btn = button(text(if launching { "启动中..." } else { "启动" }))
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
            });

        let edit_btn = button(text("配置"))
            .padding([8, 14])
            .on_press(Message::GameSelected(game.id.clone()));

        container(
            row![
                column![
                    text(game.name.clone()).size(15).font(ui_font()),
                    text(game.exe.clone())
                        .size(11)
                        .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
                    text(game.scale_label())
                        .size(11)
                        .color(Color::from_rgb8(0x7a, 0xaa, 0x7f)),
                ]
                .spacing(3)
                .align_x(iced::Alignment::Start),
                iced::widget::horizontal_space(),
                edit_btn,
                launch_btn,
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

    fn game_detail_view(&self) -> Element<'_, Message> {
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

        let mut body = column![
            row![back_btn, iced::widget::horizontal_space()],
            text(&draft.game_name)
                .size(20)
                .font(ui_font()),
            text("每次修改保存后，重新启动游戏即生效。")
                .size(12)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
            horizontal_rule(1),
            exe_row,
            algo_row,
            sharpness_row,
            num_input("游戏分辨率宽", draft.internal_w.clone(), Message::InternalWChanged),
            num_input("游戏分辨率高", draft.internal_h.clone(), Message::InternalHChanged),
            num_input("输出分辨率宽", draft.output_w.clone(), Message::OutputWChanged),
            num_input("输出分辨率高", draft.output_h.clone(), Message::OutputHChanged),
            text("提示：在 Niri 等平铺桌面下游戏会铺满整块显示器，输出分辨率主要影响缩放计算；KDE 浮动桌面将支持自由调整窗口尺寸实现自定义缩放。")
                .size(11)
                .color(Color::from_rgb8(0x8a, 0x8a, 0x8a)),
            fullscreen_toggle,
            framerate_input,
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

    fn settings_view(&self) -> Element<'_, Message> {
        column![
            text("设置").size(18).font(ui_font()),
            horizontal_rule(1),
            text("全局缩放配置、目录扫描、云同步设置即将上线。")
                .size(13)
                .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        ]
        .spacing(12)
        .into()
    }
}

/// Load the library, booting the daemon first if it is not running. Used for
/// both the initial load and automatic reconnect.
async fn connect_and_load() -> Result<Vec<UiGame>, String> {
    let socket = crate::config::socket_path();
    match load_games_from(&socket).await {
        Ok(games) => Ok(games),
        Err(first) => {
            let boot = socket.clone();
            let booted = tokio::task::spawn_blocking(move || crate::daemon::ensure_running(&boot))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("启动守护进程的任务失败: {e}")));
            match booted {
                Ok(()) => load_games_from(&socket).await,
                Err(_) => Err(first),
            }
        }
    }
}

async fn load_games_from(socket: &Path) -> Result<Vec<UiGame>, String> {
    let value = crate::rpc::call(socket, "game.list", None).await?;
    parse_games(&value)
}

/// Backoff for automatic reconnect attempts: 2s, 4s, 8s, 16s, capped at 30s.
fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(2u64.pow(attempt.min(4)).min(30))
}

/// Case-insensitive match against a game's name or exe path; an empty query
/// matches everything.
fn matches_query(game: &UiGame, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    game.name.to_lowercase().contains(&query) || game.exe.to_lowercase().contains(&query)
}

async fn scan_directory(socket: &Path, directory: &str) -> Result<Vec<ScanCandidate>, String> {
    let params = crate::rpc::params([("directory", Value::String(directory.to_string()))]);
    let value = crate::rpc::call(socket, "game.scan", Some(params)).await?;
    parse_scan_candidates(&value)
}

async fn add_directory(socket: &Path, directory: &str) -> Result<String, String> {
    let params = crate::rpc::params([("directory", Value::String(directory.to_string()))]);
    let value = crate::rpc::call(socket, "game.add", Some(params)).await?;

    let added = value
        .get("added")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let found = value.get("found").and_then(|v| v.as_u64()).unwrap_or(0);

    Ok(if added == 0 {
        format!("没有新游戏（发现 {found} 个目录，全部已在库中）")
    } else {
        format!("已添加 {added} 个游戏（发现 {found} 个目录）")
    })
}

async fn remove_game(socket: &Path, game_id: &str) -> Result<(), String> {
    let params = crate::rpc::params([("id", Value::String(game_id.to_string()))]);
    crate::rpc::call(socket, "game.remove", Some(params)).await?;
    Ok(())
}

/// Persist the whole edit form through the daemon, which is the single writer
/// of the config file.
async fn save_profile(draft: Draft) -> Result<(), String> {
    let profile = profile_from_draft(&draft)?;

    if draft.exe.trim().is_empty() {
        return Err("可执行文件路径不能为空".to_string());
    }

    let mut params = vec![
        ("id", Value::String(draft.game_id.clone())),
        (
            "profile",
            serde_json::to_value(&profile).map_err(|e| e.to_string())?,
        ),
    ];
    // Only send the exe path when it actually changed: the daemon rejects a
    // path whose file is missing, and a game on an unmounted drive must not
    // block a scale edit.
    if draft.exe_changed() {
        params.push(("exe_path", Value::String(draft.exe.trim().to_string())));
    }

    crate::rpc::call(
        &crate::config::socket_path(),
        "game.update",
        Some(crate::rpc::params(params)),
    )
    .await?;
    Ok(())
}

fn parse_scan_candidates(value: &Value) -> Result<Vec<ScanCandidate>, String> {
    let games = value
        .get("games")
        .and_then(|g| g.as_array())
        .ok_or_else(|| "守护进程返回格式异常".to_string())?;

    Ok(games
        .iter()
        .map(|g| ScanCandidate {
            id: g
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            name: g
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("未知")
                .to_string(),
            exe: g
                .get("exe_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            is_new: g.get("is_new").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect())
}

fn profile_from_draft(draft: &Draft) -> Result<ScaleProfile, String> {
    let algorithm = ScaleAlgorithm::from_label(&draft.algo)
        .ok_or_else(|| format!("未知缩放算法: {}", draft.algo))?
        .with_sharpness(draft.sharpness);

    Ok(ScaleProfile {
        name: draft.profile_name.clone(),
        algorithm,
        internal_width: parse_u32(&draft.internal_w, "游戏分辨率宽")?,
        internal_height: parse_u32(&draft.internal_h, "游戏分辨率高")?,
        output_width: parse_u32(&draft.output_w, "输出分辨率宽")?,
        output_height: parse_u32(&draft.output_h, "输出分辨率高")?,
        framerate_limit: if draft.framerate.trim().is_empty() {
            None
        } else {
            Some(parse_u32(&draft.framerate, "帧率限制")?)
        },
        force_fullscreen: draft.fullscreen,
    })
}

fn parse_u32(s: &str, label: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} 必须是正整数"))
}

fn parse_games(value: &Value) -> Result<Vec<UiGame>, String> {
    let games = value
        .get("games")
        .and_then(|g| g.as_array())
        .ok_or_else(|| "守护进程返回格式异常".to_string())?;

    games
        .iter()
        .map(|g| {
            // The daemon sends the whole GameConfig, so read `scale_profile`
            // directly instead of a hand-picked subset.
            let scale = g.get("scale_profile");
            let algorithm = scale.and_then(|s| s.get("algorithm"));

            Ok(UiGame {
                id: g
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: g
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知")
                    .to_string(),
                exe: g
                    .get("exe_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                profile_name: scale
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("默认")
                    .to_string(),
                algo: algorithm
                    .and_then(algo_label)
                    .unwrap_or_else(|| "Fsr".to_string()),
                sharpness: algorithm.and_then(algo_sharpness).unwrap_or(2),
                internal: (
                    u32_field(scale, "internal_width").unwrap_or(0),
                    u32_field(scale, "internal_height").unwrap_or(0),
                ),
                output: (
                    u32_field(scale, "output_width").unwrap_or(0),
                    u32_field(scale, "output_height").unwrap_or(0),
                ),
                fullscreen: scale
                    .and_then(|s| s.get("force_fullscreen"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                framerate: scale
                    .and_then(|s| s.get("framerate_limit"))
                    .and_then(|v| v.as_u64().map(|f| f as u32)),
            })
        })
        .collect()
}

fn u32_field(parent: Option<&Value>, key: &str) -> Option<u32> {
    parent?.get(key)?.as_u64().map(|v| v as u32)
}

/// Algorithm label from the serialized form. Serde tags struct variants as an
/// object (`{"Fsr": {"sharpness": 2}}`) but unit variants as a bare string
/// (`"Integer"`), so both shapes must be handled.
fn algo_label(v: &Value) -> Option<String> {
    if let Some(label) = v.as_str() {
        return Some(label.to_string());
    }
    let obj = v.as_object()?;
    obj.keys().next().cloned()
}

/// `sharpness` of the serialized algorithm, when it has one.
fn algo_sharpness(v: &Value) -> Option<u32> {
    let obj = v.as_object()?;
    let (_k, val) = obj.iter().next()?;
    val.get("sharpness")?.as_u64().map(|s| s as u32)
}

pub fn run() -> anyhow::Result<()> {
    // Niri + wgpu Vulkan swapchain constantly reports SurfaceError::Outdated,
    // producing an ERROR log storm (33k lines in 6s). Force GL/EGL by default;
    // an explicit WGPU_BACKEND env from the user still takes precedence.
    if std::env::var_os("WGPU_BACKEND").is_none() {
        unsafe { std::env::set_var("WGPU_BACKEND", "gl") };
    }

    let socket = crate::config::socket_path();

    // Make sure the daemon is up; if it cannot be started the UI still opens
    // and simply reports「未连接」.
    if let Err(e) = crate::daemon::ensure_running(&socket) {
        tracing::warn!("{e}");
    }

    iced::application("Kotori", App::update, App::view)
        .font(UI_FONT_BYTES)
        .default_font(ui_font())
        .window(iced::window::Settings {
            size: Size::new(960.0, 640.0),
            min_size: Some(Size::new(760.0, 520.0)),
            ..Default::default()
        })
        .theme(|_| Theme::Dark)
        .run_with(App::new)
        .map_err(|e| anyhow::anyhow!("UI error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn daemon_game_list() -> Value {
        json!({
            "games": [{
                "id": "demo",
                "name": "demo",
                "exe_path": "/games/demo/game.exe",
                "save_paths": [],
                "wine_prefix": null,
                "created_at": "2026-01-01T00:00:00Z",
                "scale_profile": {
                    "name": "自定义",
                    "algorithm": { "Nis": { "sharpness": 4 } },
                    "internal_width": 1920,
                    "internal_height": 1080,
                    "output_width": 2560,
                    "output_height": 1440,
                    "framerate_limit": 60,
                    "force_fullscreen": false
                }
            }]
        })
    }

    fn draft_with(algo: &str) -> Draft {
        Draft {
            game_id: "x".into(),
            game_name: "x".into(),
            profile_name: "默认".into(),
            exe: "/games/x/game.exe".into(),
            exe_original: "/games/x/game.exe".into(),
            algo: algo.into(),
            sharpness: 2,
            internal_w: "1280".into(),
            internal_h: "720".into(),
            output_w: "2560".into(),
            output_h: "1440".into(),
            fullscreen: true,
            framerate: String::new(),
        }
    }

    #[test]
    fn parses_the_full_scale_profile() {
        let games = parse_games(&daemon_game_list()).unwrap();
        let game = &games[0];
        assert_eq!(game.id, "demo");
        assert_eq!(game.profile_name, "自定义");
        assert_eq!(game.algo, "Nis");
        assert_eq!(game.sharpness, 4);
        assert_eq!(game.internal, (1920, 1080));
        assert_eq!(game.output, (2560, 1440));
        assert_eq!(game.framerate, Some(60));
        assert!(!game.fullscreen);
    }

    #[test]
    fn open_then_save_preserves_stored_values() {
        // Regression test for the silent overwrite: the draft used to start
        // from hard-coded defaults (sharpness 2 / fullscreen true / no fps).
        let game = parse_games(&daemon_game_list()).unwrap().remove(0);
        let draft = Draft::from_game(&game);

        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.name, "自定义");
        assert_eq!(profile.algorithm, ScaleAlgorithm::Nis { sharpness: 4 });
        assert_eq!(profile.framerate_limit, Some(60));
        assert!(!profile.force_fullscreen);
    }

    #[test]
    fn unit_variant_algorithms_survive_a_load_save_cycle() {
        // serde writes unit variants as a bare string (`"Integer"`). Reading
        // that as an object used to silently turn the game into FSR on save.
        let value = json!({
            "games": [{
                "id": "int",
                "name": "int",
                "exe_path": "/int.exe",
                "scale_profile": {
                    "name": "默认",
                    "algorithm": "Integer",
                    "internal_width": 640,
                    "internal_height": 480,
                    "output_width": 1280,
                    "output_height": 960
                }
            }]
        });
        let game = parse_games(&value).unwrap().remove(0);
        assert_eq!(game.algo, "Integer");

        let draft = Draft::from_game(&game);
        assert_eq!(draft.algo, "Integer");
        let profile = profile_from_draft(&draft).unwrap();
        assert_eq!(profile.algorithm, ScaleAlgorithm::Integer);
        assert_eq!(profile.internal_width, 640);
        assert_eq!(profile.output_width, 1280);
    }

    #[test]
    fn malformed_and_empty_response_are_errors() {
        assert!(parse_games(&json!({})).is_err());
        assert!(parse_games(&json!({ "games": [] })).unwrap().is_empty());
    }

    #[test]
    fn missing_optional_fields_fall_back_safely() {
        let value = json!({
            "games": [{
                "id": "x",
                "name": "x",
                "exe_path": "/x.exe",
                "scale_profile": { "algorithm": "Integer" }
            }]
        });
        let games = parse_games(&value).unwrap();
        assert_eq!(games[0].algo, "Integer");
        assert_eq!(games[0].sharpness, 2);
        assert_eq!(games[0].internal, (0, 0));
        assert!(!games[0].fullscreen);
    }

    #[test]
    fn unknown_algorithm_is_rejected_on_save() {
        assert!(profile_from_draft(&draft_with("Lanczos")).is_err());
    }

    #[test]
    fn non_numeric_resolution_is_rejected() {
        let mut draft = draft_with("Fsr");
        draft.internal_w = "abc".into();
        let err = profile_from_draft(&draft).unwrap_err();
        assert!(err.contains("游戏分辨率宽"), "{err}");
    }

    fn ui_game() -> UiGame {
        UiGame {
            id: "demo".into(),
            name: "Demo Game".into(),
            exe: "/games/demo/game.exe".into(),
            profile_name: "默认".into(),
            algo: "Fsr".into(),
            sharpness: 2,
            internal: (1280, 720),
            output: (2560, 1440),
            fullscreen: true,
            framerate: None,
        }
    }

    #[test]
    fn search_matches_name_and_path_case_insensitively() {
        let game = ui_game();
        assert!(matches_query(&game, ""), "empty query shows everything");
        assert!(matches_query(&game, "   "));
        assert!(matches_query(&game, "demo"));
        assert!(matches_query(&game, "DEMO"));
        assert!(matches_query(&game, "Game")); // name
        assert!(matches_query(&game, "games/demo")); // path
        assert!(matches_query(&game, ".exe"));
        assert!(!matches_query(&game, "nonexistent"));
    }

    #[test]
    fn retry_backoff_grows_then_caps() {
        assert_eq!(retry_delay(1), std::time::Duration::from_secs(2));
        assert_eq!(retry_delay(2), std::time::Duration::from_secs(4));
        assert_eq!(retry_delay(3), std::time::Duration::from_secs(8));
        assert_eq!(retry_delay(4), std::time::Duration::from_secs(16));
        // capped, and never overflows for a large attempt count
        assert_eq!(retry_delay(5), std::time::Duration::from_secs(16));
        assert_eq!(retry_delay(99), std::time::Duration::from_secs(16));
    }

    #[test]
    fn parses_scan_candidates() {
        let value = json!({
            "directory": "/games",
            "games": [
                { "id": "a", "name": "A", "exe_path": "/games/a/a.exe", "is_new": true },
                { "id": "b", "name": "B", "exe_path": "/games/b/b.exe", "is_new": false }
            ]
        });
        let found = parse_scan_candidates(&value).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found[0].is_new);
        assert!(!found[1].is_new);
        assert_eq!(found[1].exe, "/games/b/b.exe");

        assert!(parse_scan_candidates(&json!({})).is_err());
        assert!(
            parse_scan_candidates(&json!({ "games": [] }))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn draft_seeds_exe_and_detects_changes() {
        let game = ui_game();
        let mut draft = Draft::from_game(&game);
        assert_eq!(draft.exe, game.exe);
        assert!(
            !draft.exe_changed(),
            "opening a game must not count as an edit"
        );

        draft.exe = "/games/demo/other.exe".into();
        assert!(draft.exe_changed());

        // Whitespace-only differences are not an edit either.
        let mut padded = Draft::from_game(&game);
        padded.exe = format!("  {}  ", game.exe);
        assert!(!padded.exe_changed());
    }

    /// Building the widget tree must not panic in any reachable state. This
    /// covers the empty-list / no-search-hit branches of the new pages.
    #[test]
    fn views_construct_for_every_tab_and_state() {
        let (mut app, _task) = App::new();

        // Library: empty, populated, filtered, no match.
        app.tab = Tab::Games;
        let _ = app.view();
        app.games = vec![ui_game()];
        let _ = app.view();
        app.search = "demo".into();
        let _ = app.view();
        app.search = "zzz".into();
        let _ = app.view();
        app.search.clear();

        // Detail page, with and without the delete confirmation.
        app.selected = Some("demo".into());
        app.draft = Some(Draft::from_game(&ui_game()));
        let _ = app.view();
        app.confirm_delete = true;
        let _ = app.view();

        // Add page: before a scan, with results, with an empty result.
        app.tab = Tab::Add;
        app.selected = None;
        app.draft = None;
        app.confirm_delete = false;
        let _ = app.view();
        app.scan_results = Some(vec![ScanCandidate {
            id: "a".into(),
            name: "A".into(),
            exe: "/games/a/a.exe".into(),
            is_new: true,
        }]);
        let _ = app.view();
        app.scan_results = Some(Vec::new());
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

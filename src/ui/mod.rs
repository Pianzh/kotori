use std::path::PathBuf;

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
    Settings,
}

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
    AlgoChanged(String),
    SharpnessChanged(f32),
    InternalWChanged(String),
    InternalHChanged(String),
    OutputWChanged(String),
    OutputHChanged(String),
    FullscreenToggled(bool),
    FramerateChanged(String),
    SaveProfile,
    ProfileSaved(Result<(), String>),
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
            },
            Task::perform(async { load_games().await }, Message::GamesLoaded),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabChanged(tab) => {
                self.tab = tab;
                self.selected = None;
                self.draft = None;
                self.error = None;
                Task::none()
            }
            Message::Refresh => {
                self.error = None;
                Task::perform(async { load_games().await }, Message::GamesLoaded)
            }
            Message::GamesLoaded(Ok(games)) => {
                self.games = games;
                self.loading = false;
                self.daemon_connected = Some(true);
                self.error = None;
                Task::none()
            }
            Message::GamesLoaded(Err(e)) => {
                self.loading = false;
                self.daemon_connected = Some(false);
                self.error = Some(e);
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
                    return Task::perform(async { load_games().await }, |r| {
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
            Some(false) => ("未连接", Color::from_rgb8(0xe5, 0x39, 0x35)),
            None => ("检测中...", Color::from_rgb8(0x9e, 0x9e, 0x9e)),
        };

        container(
            column![
                text("Kotori").size(20).font(ui_font()),
                horizontal_rule(1),
                self.nav_item(Tab::Games, "游戏库"),
                self.nav_item(Tab::Settings, "设置"),
                iced::widget::Space::with_height(Length::Fill),
                row![
                    text("\u{25CF}").size(12).color(daemon_status.1),
                    text(daemon_status.0)
                        .size(12)
                        .color(Color::from_rgb8(0x9e, 0x9e, 0x9e)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
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
        let count = format!("游戏库 ({})", self.games.len());
        let refresh_btn = button(text(if self.loading {
            "加载中..."
        } else {
            "刷新"
        }))
        .on_press(Message::Refresh);

        let mut list = column![
            row![
                text(count).size(18).font(ui_font()),
                iced::widget::horizontal_space(),
                refresh_btn,
            ]
            .align_y(iced::Alignment::Center),
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
                    "没有已配置的游戏。可在终端执行 `kotori scan <目录>` 添加。"
                })
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
                        self.games
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

async fn load_games() -> Result<Vec<UiGame>, String> {
    let socket = crate::config::socket_path();
    let value = crate::rpc::call(&socket, "game.list", None).await?;
    parse_games(&value)
}

async fn save_profile(draft: Draft) -> Result<(), String> {
    let profile = profile_from_draft(&draft)?;
    let mut config = crate::config::load().map_err(|e| e.to_string())?;

    let game = config
        .games
        .get_mut(&draft.game_id)
        .ok_or_else(|| format!("配置中找不到游戏: {}", draft.game_id))?;
    game.scale_profile = profile;

    crate::config::save(&config).map_err(|e| e.to_string())?;

    // Tell the daemon to reload config (ignore failures – daemon may be down).
    let _ = crate::rpc::call(&crate::config::socket_path(), "config.reload", None).await;
    Ok(())
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
}

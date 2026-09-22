//! Plain data shared by the UI. The message/page enums live in
//! [`super::message`]; everything a daemon answer is turned into lives here.
//!
//! 按「这块状态归谁」拆成子模块:本机环境(`environment`)、游戏与它的草稿
//! (`game`)、云同步(`sync`)、会话与连接参数(`session`)。每个子模块顶部写清
//! 它负责什么、为什么和邻居分开。
//!
//! 本文件只是门面:本来 `pub` 的六个类型在这里重新 `pub use`,其余按原来的
//! `pub(super)` 可见性重导出 —— `crate::ui::model::X` 这些路径照旧可用,调用方
//! 一行都不用改。

mod environment;
mod game;
mod picker;
mod session;
mod sync;

pub use environment::{ConfigSource, WineStatus};
pub use game::{SavePathDraft, UiGame};
pub use picker::ProcessRow;
pub use session::SessionInfo;
pub use sync::{PairingRow, PairingState, SyncGameRow, SyncStatus};

pub(super) use environment::{EnvCheck, Environment};
pub(super) use game::{AUTOSAVE_DEBOUNCE, Draft, SAVE_PATH_KINDS, SaveAttempt};
pub(super) use picker::ProcessPicker;
pub(super) use session::{MAX_AUTO_RETRIES, STATUS_POLL};
pub(super) use sync::{CredentialStore, SyncForm, engine_switched_note};

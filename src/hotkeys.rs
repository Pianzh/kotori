//! Global hotkeys for runtime scaling.
//!
//! This module owns exactly one half of the feature: **the trigger**. When the
//! user presses the key they bound in their desktop's settings, the portal tells
//! us which [`ScaleAction`] fired, and we hand it to whoever registered a sink
//! (the daemon, which applies it to the running games — see `crate::scale::x11`).
//!
//! Why the portal at all: a Wayland client cannot watch for global keys by
//! itself, and `org.freedesktop.portal.GlobalShortcuts` is the supported way to
//! ask the desktop for some. The desktop owns the actual key bindings, so the
//! user can change them without kotori, and no key press is ever swallowed from
//! the game.
//!
//! Why *only* the trigger: the previous design also pressed gamescope's own
//! shortcuts for the user, through `org.freedesktop.portal.RemoteDesktop`. That
//! could not work:
//!
//! * gamescope's nested (Wayland) backend never recognises those chords —
//!   `CWaylandInputThread::HandleKey` compares the raw Wayland keycode against
//!   evdev `KEY_*` constants, i.e. it is off by the 8 the Wayland protocol adds,
//!   so `Super+U` and friends fall through to the game (measured: an mpv client
//!   behind gamescope received `Meta+y` verbatim);
//! * injected keys go to whatever window has keyboard focus, which the portal
//!   does not guarantee — and the consent dialog steals it once;
//! * it needed a second consent dialog (`RemoteDesktop`) before the first
//!   keypress could do anything.
//!
//! So the trigger stays here and the effect moved to where it can be exact: the
//! properties gamescope watches on its own Xwayland.
//!
//! One thing has to happen before any portal call: the portal needs an **app
//! id** for kotori. It works one out for apps a launcher started (from the
//! systemd unit in the cgroup), but a daemon that `ensure_running` spawned from
//! a terminal has none, and `GlobalShortcuts` refuses to work without one
//! ("An app id is required"). [`ensure_app_id`] registers [`APP_ID`] through
//! `org.freedesktop.host.portal.Registry`, the documented way for an
//! unsandboxed application to say who it is — and the portal looks that id up
//! in the desktop-file database, so `assets/<APP_ID>.desktop` has to be
//! installed or registration fails with "App info not found".
//!
//! Nothing in here may block launching a game: every failure is logged and the
//! feature degrades to "no hotkeys", which is exactly the state before it
//! existed.

use std::time::Duration;

use ashpd::desktop::CreateSessionOptions;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;

use crate::scale::ScaleAction;

/// How close two activations of the same shortcut have to be to count as one
/// press delivered twice.
///
/// A human cannot press a key twice in this window, so nothing legitimate is
/// lost — while a duplicate delivery is exactly this close: the desktop sent
/// two `Activated` 61µs apart in the observed case.
const DUPLICATE_WINDOW: Duration = Duration::from_millis(50);

/// The app id the portal knows kotori by.
///
/// It has to be the basename of the `.desktop` file we ship (`assets/`), and it
/// is also what the desktop stores the shortcuts under. Change one, change both
/// — there is a test for that.
pub const APP_ID: &str = "io.github.kotori";

/// Set this to keep the portal out of the picture entirely.
///
/// The end-to-end tests launch games through a *real* daemon on the developer's
/// own desktop, so without a switch they would pop a consent dialog on every
/// `cargo test`.
const DISABLE_ENV: &str = "KOTORI_NO_HOTKEYS";

fn disabled() -> bool {
    matches!(std::env::var(DISABLE_ENV), Ok(value) if !value.is_empty() && value != "0")
}

/// Anything that can go wrong on the way to the portal.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("portal 不可用（没有 D-Bus、没有后端，或用户拒绝了授权）：{0}")]
    Portal(#[from] ashpd::Error),

    #[error("portal 认不出 kotori 这个程序，热键无法注册：{0}")]
    AppId(String),
}

/// Tell the portal which application this is.
///
/// Unsandboxed applications normally have no app id at all — the portal only
/// works one out for apps it can trace back to a launcher's systemd unit. That
/// is enough for the GUI opened from the menu and not for a daemon that
/// `ensure_running` spawned from a terminal, so we say it ourselves through
/// `org.freedesktop.host.portal.Registry`.
///
/// The portal requires this **before any other portal call on this D-Bus
/// connection**, and ashpd shares one connection process-wide, so this is the
/// first thing every portal path in this module does. Inside a sandbox
/// (flatpak/snap) there is nothing to do and ashpd returns early.
async fn ensure_app_id() -> Result<(), PortalError> {
    static REGISTERED: std::sync::LazyLock<tokio::sync::OnceCell<Result<(), String>>> =
        std::sync::LazyLock::new(tokio::sync::OnceCell::new);

    let outcome = REGISTERED
        .get_or_init(|| async {
            let app_id = ashpd::AppID::try_from(APP_ID)
                .map_err(|err| format!("应用 ID {APP_ID} 本身不合法：{err}"))?;
            ashpd::register_host_app(app_id).await.map_err(|err| {
                format!(
                    "{err}（portal 要按名字在桌面文件库里找 kotori：\n\
                     ① 先把 kotori 装上 PATH（cargo install --path .）\n\
                     ② 再把 assets/{APP_ID}.desktop 装进 ~/.local/share/applications/\n\
                     ③ 重启守护进程。注意 Exec 指向的程序不存在时，\
                     GIO 会把整份桌面文件当作不存在）"
                )
            })
        })
        .await;

    match outcome {
        Ok(()) => Ok(()),
        Err(message) => Err(PortalError::AppId(message.clone())),
    }
}

/// Where a fired hotkey goes.
///
/// The daemon owns the scale engine, so it passes a closure in when it asks for
/// the hotkeys. Keeping it a parameter rather than a global means tests cannot
/// leave a sink behind for each other.
pub type ActionSink = std::sync::Arc<dyn Fn(ScaleAction) + Send + Sync>;

/// The live service, once the portal has granted the shortcuts.
static SERVICE: std::sync::OnceLock<std::sync::Arc<Hotkeys>> = std::sync::OnceLock::new();

/// Whether an attempt is in flight, and — when the last one failed — why.
///
/// A bare `false` in `daemon.status` would leave a user with a dead feature and
/// nothing to act on, which is exactly the kind of silence this project tries
/// not to ship.
#[derive(Default)]
struct State {
    requested: bool,
    error: Option<String>,
    /// Granted by the desktop but with no key behind it. Pressing nothing is
    /// what the user experiences, and silence about it is how a working feature
    /// comes to look broken.
    unbound: Vec<String>,
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State {
    requested: false,
    error: None,
    unbound: Vec::new(),
});

/// Run `f` on the state, ignoring poisoning: a panic elsewhere has no bearing on
/// two booleans, and refusing to answer would be worse.
fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    match STATE.lock() {
        Ok(mut state) => f(&mut state),
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

fn set_error(message: String) {
    with_state(|state| state.error = Some(message));
}

/// Has this activation already been handled a moment ago?
///
/// See [`DUPLICATE_WINDOW`]: the desktop may deliver one press more than once,
/// and for a toggle that means the user sees nothing happen at all.
fn is_duplicate(
    last: Option<&(String, std::time::Instant)>,
    id: &str,
    now: std::time::Instant,
) -> bool {
    match last {
        Some((last_id, at)) => last_id == id && now.duration_since(*at) < DUPLICATE_WINDOW,
        None => false,
    }
}

/// Did the desktop grant a shortcut without binding a key to it?
///
/// The trigger comes back as free-form text, so this only reads what desktops
/// actually send: KDE answers "none", others may answer nothing at all.
fn looks_unbound(trigger: &str) -> bool {
    let trigger = trigger.trim();
    trigger.is_empty() || trigger.eq_ignore_ascii_case("none")
}

/// Where the user has to go to give a shortcut a key.
///
/// Since no desktop is obliged to read our hint, this is a real step for the
/// user — and advice for the wrong desktop is worse than none (ADR-011).
pub fn assign_hint() -> String {
    assign_hint_for(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

fn assign_hint_for(desktop: &str) -> String {
    let desktop = desktop.to_ascii_lowercase();
    if desktop.contains("kde") {
        "系统设置 → 快捷键 → kotori，给要用的那几条各指定一个按键".to_string()
    } else if desktop.contains("gnome") {
        "设置 → 键盘 → 查看及自定义快捷键，给 kotori 的条目指定按键".to_string()
    } else {
        "在系统的快捷键设置里给 kotori 的条目指定按键".to_string()
    }
}

/// What the daemon can tell a user (or the CLI) about the hotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyStatus {
    pub requested: bool,
    pub ready: bool,
    pub error: Option<String>,
    /// Shortcut ids the desktop granted without binding a key to them.
    pub unbound: Vec<String>,
    /// Where this desktop keeps its shortcut settings, for the user to act on.
    pub assign_hint: String,
}

/// Current hotkey state, for `daemon.status`.
pub fn status() -> HotkeyStatus {
    HotkeyStatus {
        requested: with_state(|state| state.requested),
        ready: ready(),
        error: with_state(|state| state.error.clone()),
        unbound: with_state(|state| state.unbound.clone()),
        assign_hint: assign_hint(),
    }
}

/// Ask the portal for the hotkeys, then listen for them.
///
/// Runs in its own task on purpose: binding the shortcuts pops a consent
/// dialog, and launching a game must never wait for a human to click it.
/// A missing portal is a supported state — the caller gets no error, just no
/// hotkeys.
pub fn spawn(sink: ActionSink) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match Hotkeys::register().await {
            Ok(hotkeys) => {
                let hotkeys = std::sync::Arc::new(hotkeys);
                // Published before the loop so `status` can report the truth as
                // soon as the shortcuts exist. `set` failing would mean another
                // task got here first, which `request_once` rules out.
                let _ = SERVICE.set(std::sync::Arc::clone(&hotkeys));
                hotkeys.run(sink).await;
            }
            Err(err) => {
                set_error(err.to_string());
                tracing::warn!("运行时缩放热键不可用，游戏不受影响：{err}");
            }
        }
    })
}

/// Request the hotkeys, unless they are already granted or a request is in
/// flight. Returns whether this call was the one that started it.
///
/// A previous *failed* attempt may be retried: the consent dialog appears while
/// a game is starting (easy to miss, easy to dismiss by accident) and a daemon
/// lives for weeks, so one stray click must not disable the feature until the
/// next restart.
pub fn request_once(sink: ActionSink) -> bool {
    if disabled() {
        set_error(format!("运行时缩放热键被 {DISABLE_ENV} 关闭"));
        return false;
    }
    if ready() {
        return false;
    }
    let start = with_state(|state| {
        if state.requested && state.error.is_none() {
            return false; // already in flight
        }
        state.requested = true;
        state.error = None;
        state.unbound.clear();
        true
    });
    if !start {
        return false;
    }
    spawn(sink);
    true
}

/// Has the portal granted the hotkeys?
pub fn ready() -> bool {
    SERVICE.get().is_some()
}

/// The registered shortcut service.
struct Hotkeys {
    shortcuts: GlobalShortcuts,
}

impl Hotkeys {
    /// Bind the shortcuts. This is the call that pops the portal's consent
    /// dialog listing every action below.
    async fn register() -> Result<Self, PortalError> {
        ensure_app_id().await?;
        let shortcuts = GlobalShortcuts::new().await?;
        let session = shortcuts
            .create_session(CreateSessionOptions::default())
            .await?;

        let wanted: Vec<NewShortcut> = ScaleAction::ALL
            .iter()
            .map(|action| {
                NewShortcut::new(action.id(), action.description())
                    .preferred_trigger(action.preferred_trigger())
            })
            .collect();

        let bound = match shortcuts
            .bind_shortcuts(&session, &wanted, None, Default::default())
            .await
            .map_err(PortalError::from)
            .and_then(|request| request.response().map_err(PortalError::from))
        {
            Ok(bound) => bound,
            Err(err) => {
                // A session whose `BindShortcuts` the user rejected is *not*
                // inert: KDE leaves the shortcuts registered and keeps
                // relaying activations to it, so the next attempt makes every
                // action fire twice (measured: two `Activated`, same instant,
                // one per session — which turns each toggle into a no-op).
                // Close it instead of leaving that behind.
                if let Err(close_err) = session.close().await {
                    tracing::warn!("关掉被拒绝的热键会话失败（下次注册可能重复触发）：{close_err}");
                }
                return Err(err);
            }
        };

        let granted: Vec<&str> = bound.shortcuts().iter().map(|s| s.id()).collect();
        for action in ScaleAction::ALL {
            if !granted.contains(&action.id()) {
                tracing::warn!(
                    "portal 没有授予热键 {}（{}）",
                    action.id(),
                    action.description()
                );
            }
        }

        // Being *granted* is not the same as being *usable*. BindShortcuts
        // returns what each shortcut is actually triggered by, and a desktop is
        // free to hand them all back with nothing bound — KDE does exactly that
        // and leaves the assignment to its own settings. Saying "registered"
        // here would be a lie the user only finds out by pressing a key that
        // does nothing.
        for shortcut in bound.shortcuts() {
            tracing::info!(
                "热键 {} 的触发键：portal 报告为 “{}”",
                shortcut.id(),
                shortcut.trigger_description()
            );
        }
        let unbound: Vec<String> = bound
            .shortcuts()
            .iter()
            .filter(|shortcut| looks_unbound(shortcut.trigger_description()))
            .map(|shortcut| shortcut.id().to_string())
            .collect();
        if !unbound.is_empty() {
            tracing::warn!(
                "这些动作还没有按键，按了不会有反应：{} —— {}",
                unbound.join(", "),
                assign_hint()
            );
        }
        with_state(|state| state.unbound = unbound);
        tracing::info!("运行时缩放热键已注册：{}", granted.join(", "));

        Ok(Self { shortcuts })
    }

    /// Wait for triggers and hand them to the sink.
    async fn run(self: std::sync::Arc<Self>, sink: ActionSink) {
        let mut stream = match self.shortcuts.receive_activated().await {
            Ok(stream) => Box::pin(stream),
            Err(err) => {
                set_error(format!("无法监听热键触发：{err}"));
                tracing::warn!("无法监听热键触发：{err}");
                return;
            }
        };

        // The last activation acted on. Kept locally: this loop is the only
        // consumer, and it is the thing that must stay single.
        let mut last: Option<(String, std::time::Instant)> = None;

        while let Some(activated) = stream.next().await {
            let Some(action) = ScaleAction::from_id(activated.shortcut_id()) else {
                continue;
            };
            let now = std::time::Instant::now();
            if is_duplicate(last.as_ref(), activated.shortcut_id(), now) {
                tracing::warn!(
                    "忽略重复投递的热键事件 {}（同一个按键被送来了两次）",
                    activated.shortcut_id()
                );
                continue;
            }
            last = Some((activated.shortcut_id().to_string(), now));
            tracing::info!("缩放热键 {} → {}", action.id(), action.description());
            sink(action);
        }
        set_error("portal 会话已关闭，缩放热键失效（重启守护进程可重新申请）".to_string());
        tracing::warn!("热键监听结束（portal 会话关闭）");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_that_is_registered_can_fire() {
        // The portal id is the only link between a shortcut and an action, so a
        // typo here would register a hotkey that can never do anything.
        for action in ScaleAction::ALL {
            assert_eq!(ScaleAction::from_id(action.id()), Some(action));
        }
        assert_eq!(ScaleAction::from_id("nope"), None);
    }

    #[test]
    fn ids_and_triggers_are_unique() {
        let ids: Vec<&str> = ScaleAction::ALL.iter().map(|a| a.id()).collect();
        let triggers: Vec<&str> = ScaleAction::ALL
            .iter()
            .filter_map(|a| a.preferred_trigger())
            .collect();
        for set in [&ids, &triggers] {
            let mut seen = set.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), set.len(), "重复项：{set:?}");
        }
    }

    #[test]
    fn only_the_two_that_matter_mid_game_come_with_a_default() {
        // Everything is registered so that everything *can* be bound, but only the
        // two a player reaches for without leaving the game are suggested: the rest
        // are bound by choice, from the desktop's shortcut settings or kotori's own
        // page.
        assert_eq!(
            ScaleAction::ToggleScale.preferred_trigger(),
            Some("<Shift><Alt>q")
        );
        assert_eq!(
            ScaleAction::ToggleFullscreen.preferred_trigger(),
            Some("<Shift><Alt>a")
        );
        for action in ScaleAction::ALL {
            if !matches!(
                action,
                ScaleAction::ToggleScale | ScaleAction::ToggleFullscreen
            ) {
                assert_eq!(
                    action.preferred_trigger(),
                    None,
                    "{} 不该有默认键",
                    action.id()
                );
            }
        }
    }

    #[test]
    fn the_same_press_delivered_twice_is_only_acted_on_once() {
        let now = std::time::Instant::now();
        let last = ("toggle-nis".to_string(), now);
        assert!(is_duplicate(
            Some(&last),
            "toggle-nis",
            now + Duration::from_micros(61)
        ));
        // A different action is never a duplicate...
        assert!(!is_duplicate(Some(&last), "toggle-fsr", now));
        // ...and neither is a genuine second press a moment later.
        assert!(!is_duplicate(
            Some(&last),
            "toggle-nis",
            now + DUPLICATE_WINDOW + Duration::from_millis(1)
        ));
        assert!(!is_duplicate(None, "toggle-nis", now));
    }

    #[test]
    fn a_shortcut_granted_without_a_key_is_not_called_usable() {
        assert!(looks_unbound(""));
        assert!(looks_unbound("   "));
        assert!(looks_unbound("none"));
        assert!(looks_unbound("None"));
        assert!(!looks_unbound("<Shift><Alt>q"));
        assert!(!looks_unbound("Shift+Alt+Q"));

        // The hint has to match the desktop that is actually running.
        assert!(assign_hint_for("KDE").contains("系统设置"));
        assert!(assign_hint_for("ubuntu:GNOME").contains("键盘"));
        assert!(!assign_hint_for("sway").contains("系统设置"));
    }

    #[test]
    fn the_app_id_matches_the_desktop_file_we_ship() {
        // The portal looks the id up in the desktop-file database: if the file
        // name and the id drift apart, registration fails with "App info not
        // found" and the whole feature degrades with no clue as to why.
        let expected = format!("{APP_ID}.desktop");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(&expected);
        assert!(path.is_file(), "assets/{expected} 不见了：{path:?}");
        assert!(
            ashpd::AppID::try_from(APP_ID).is_ok(),
            "{APP_ID} 不是合法的应用 ID"
        );
    }

    #[test]
    fn descriptions_name_the_action_not_the_key_that_triggers_it() {
        // The description becomes the shortcut's *name* in the desktop's
        // shortcut settings, where the user picks the key. A chord in the name
        // reads as "press this", so it must not contain one.
        for action in ScaleAction::ALL {
            assert!(!action.description().is_empty());
            assert!(
                !action.description().contains("Super") && !action.description().contains('+'),
                "{} 的描述里有按键：{}",
                action.id(),
                action.description()
            );
        }
    }

    #[test]
    fn a_fresh_daemon_reports_the_hotkeys_as_unavailable() {
        // No test registers the portal service, so this is exactly the state a
        // fresh daemon is in.
        assert!(!ready());
        let status = status();
        assert!(!status.ready);
        assert!(!status.requested, "没有测试调用过 request_once");
        assert!(status.error.is_none());
        assert!(status.unbound.is_empty());
    }
}

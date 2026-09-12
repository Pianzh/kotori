//! Hotkey-driven **runtime** scaling.
//!
//! gamescope exposes no programmable API for its scaler: the only commands its
//! `gamescope_private` interface accepts are `help`, `screenshot`, `shutdown`,
//! `version`, `focus_info`, `backend_*`, `debug_*` and `set_look`. The only way
//! to change scaling while a game runs is therefore to press the shortcuts
//! gamescope listens for itself:
//!
//! | shortcut | effect |
//! |---|---|
//! | `Super+U` | toggle FSR upscaling |
//! | `Super+Y` | toggle NIS upscaling |
//! | `Super+N` | toggle nearest-neighbour |
//! | `Super+B` | back to bilinear (no upscale filter) |
//! | `Super+I` / `Super+O` | one step softer / one step sharper |
//! | `Super+F` | toggle fullscreen |
//!
//! That list is read off `CWaylandInputThread::HandleKey` in
//! `src/Backends/WaylandBackend.cpp` — the nested backend, which is the one
//! kotori uses. The filter changes on key *release* and the key is swallowed
//! rather than forwarded to the game.
//!
//! A Wayland client cannot synthesise keys — KWin implements neither
//! `zwp_virtual_keyboard_manager_v1` nor anything equivalent — but the
//! `org.freedesktop.portal.RemoteDesktop` portal can inject them, and
//! `org.freedesktop.portal.GlobalShortcuts` lets kotori own the triggers.
//!
//! Both portals ask the user for consent, so:
//!
//! * the shortcut session is requested when a game session starts, i.e. exactly
//!   when the hotkeys become useful, and
//! * the injection session is requested on the first trigger, with
//!   [`PersistMode::ExplicitlyRevoked`] so the dialog appears once and the
//!   returned restore token is reused from then on.
//!
//! Nothing in here may block launching a game: every failure is logged and the
//! feature degrades to "no hotkeys", which is exactly the state before it
//! existed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::remote_desktop::{
    DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions, StartOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use futures_util::StreamExt;
use tokio::sync::Mutex;

/// XKB keysym of the left Super/Meta key.
const KEYSYM_SUPER_L: i32 = 0xffeb;

/// Space left between injected keys. gamescope sees the chord as ordinary key
/// events and matches the modifier against its own state, so the press/release
/// order has to survive the round trip through the compositor.
const KEY_GAP: Duration = Duration::from_millis(20);

/// File under the data dir that remembers the portal's consent token.
const RESTORE_TOKEN_FILE: &str = "portal-restore-token";

/// Set this to keep the portal out of the picture entirely.
///
/// The end-to-end tests launch games through a *real* daemon on the developer's
/// own desktop, so without a switch they would pop a consent dialog on every
/// `cargo test`.
const DISABLE_ENV: &str = "KOTORI_NO_HOTKEYS";

fn disabled() -> bool {
    matches!(std::env::var(DISABLE_ENV), Ok(value) if !value.is_empty() && value != "0")
}

/// One runtime scaling action — that is, one of gamescope's own shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamescopeAction {
    ToggleFsr,
    ToggleNis,
    ToggleNearest,
    ToggleLinear,
    Soften,
    Sharpen,
    ToggleFullscreen,
}

impl GamescopeAction {
    /// Every action kotori registers, in the order the portal lists them.
    pub const ALL: [Self; 7] = [
        Self::ToggleFsr,
        Self::ToggleNis,
        Self::ToggleNearest,
        Self::ToggleLinear,
        Self::Soften,
        Self::Sharpen,
        Self::ToggleFullscreen,
    ];

    /// Stable id: the portal shortcut id, and what the RPC/CLI layer sends.
    pub fn id(self) -> &'static str {
        match self {
            Self::ToggleFsr => "toggle-fsr",
            Self::ToggleNis => "toggle-nis",
            Self::ToggleNearest => "toggle-nearest",
            Self::ToggleLinear => "toggle-linear",
            Self::Soften => "soften",
            Self::Sharpen => "sharpen",
            Self::ToggleFullscreen => "toggle-fullscreen",
        }
    }

    /// Look an id up as it comes back from the portal.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// Shown in the portal's consent dialog, so it is user-facing text.
    ///
    /// The two sharpness steps name the *effect*, which is the opposite way
    /// round from gamescope's own help text — see [`Self::for_sharpness_delta`].
    pub fn description(self) -> &'static str {
        match self {
            Self::ToggleFsr => "开启/关闭 FSR 放大（Super+U）",
            Self::ToggleNis => "开启/关闭 NIS 放大（Super+Y）",
            Self::ToggleNearest => "切换最近邻放大（Super+N）",
            Self::ToggleLinear => "切回双线性过滤，关掉放大滤镜（Super+B）",
            Self::Soften => "降低锐度 1 级（Super+I）",
            Self::Sharpen => "提高锐度 1 级（Super+O）",
            Self::ToggleFullscreen => "切换游戏全屏（Super+F）",
        }
    }

    /// Trigger suggested to the portal. The user may rebind it in its dialog.
    pub fn preferred_trigger(self) -> &'static str {
        match self {
            Self::ToggleFsr => "<Control><Alt>u",
            Self::ToggleNis => "<Control><Alt>y",
            Self::ToggleNearest => "<Control><Alt>n",
            Self::ToggleLinear => "<Control><Alt>b",
            Self::Soften => "<Control><Alt>i",
            Self::Sharpen => "<Control><Alt>o",
            Self::ToggleFullscreen => "<Control><Alt>f",
        }
    }

    /// Keysym gamescope expects, i.e. the letter of its own shortcut.
    pub fn keysym(self) -> i32 {
        match self {
            Self::ToggleFsr => 0x75,        // u
            Self::ToggleNis => 0x79,        // y
            Self::ToggleNearest => 0x6e,    // n
            Self::ToggleLinear => 0x62,     // b
            Self::Soften => 0x69,           // i
            Self::Sharpen => 0x6f,          // o
            Self::ToggleFullscreen => 0x66, // f
        }
    }

    /// The chord to inject: hold Super, tap the key, release Super.
    ///
    /// Order matters — a released modifier would not apply to the key.
    pub fn sequence(self) -> [(i32, KeyState); 4] {
        [
            (KEYSYM_SUPER_L, KeyState::Pressed),
            (self.keysym(), KeyState::Pressed),
            (self.keysym(), KeyState::Released),
            (KEYSYM_SUPER_L, KeyState::Released),
        ]
    }

    /// gamescope's own chord, for logs, docs and error messages.
    pub fn shortcut_hint(self) -> &'static str {
        match self {
            Self::ToggleFsr => "Super+U",
            Self::ToggleNis => "Super+Y",
            Self::ToggleNearest => "Super+N",
            Self::ToggleLinear => "Super+B",
            Self::Soften => "Super+I",
            Self::Sharpen => "Super+O",
            Self::ToggleFullscreen => "Super+F",
        }
    }

    /// Which key nudges kotori's sharpness scale (0 = softest, 5 = sharpest,
    /// see `config::MAX_SHARPNESS`) by `delta` steps.
    ///
    /// The direction is the opposite of what gamescope's `--help` says, and the
    /// two statements there even contradict each other: `--sharpness` is
    /// documented as "0 (max) to 20 (min)", while `Super+I` is documented as
    /// "increase FSR sharpness by 1" although it does
    /// `g_upscaleFilterSharpness + 1`. The code is the authority — gamescope
    /// hands that number to RCAS as `sharpness / 10`, and FSR's own header says
    /// "0.0 := maximum sharpness, to N>0 … reduction of sharpness"
    /// (`src/shaders/ffx_fsr1.h`). So the *number* is a softness: `Super+I`
    /// softens, `Super+O` sharpens.
    pub fn for_sharpness_delta(delta: i32) -> Option<Self> {
        match delta {
            d if d > 0 => Some(Self::Sharpen),
            d if d < 0 => Some(Self::Soften),
            _ => None,
        }
    }
}

/// Anything that can go wrong on the way to the portal.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("portal 不可用（没有 D-Bus、没有后端，或用户拒绝了授权）：{0}")]
    Portal(#[from] ashpd::Error),

    #[error("授权令牌 {path} 读写失败：{source}")]
    Token {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(
        "运行时缩放热键还没就绪：启动一次游戏让 portal 弹授权框，或者直接按 gamescope 自带热键"
    )]
    NotReady,
}

/// Where the "already consented" token lives inside the data dir.
pub fn restore_token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RESTORE_TOKEN_FILE)
}

fn read_restore_token(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let token = raw.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn write_restore_token(path: &Path, token: &str) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, token)?;
    // The token grants input injection to anyone who can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Registers the scaling hotkeys and injects gamescope's shortcuts when they
/// fire. Best-effort by design: call it once and ignore the result.
pub struct Hotkeys {
    remote: RemoteDesktop,
    shortcuts: GlobalShortcuts,
    desktop_session: Mutex<Option<Session<RemoteDesktop>>>,
    token_path: PathBuf,
}

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
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State {
    requested: false,
    error: None,
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

/// What the daemon can tell a user (or the CLI) about the hotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyStatus {
    pub requested: bool,
    pub ready: bool,
    pub error: Option<String>,
}

/// Current hotkey state, for `daemon.status`.
pub fn status() -> HotkeyStatus {
    HotkeyStatus {
        requested: with_state(|state| state.requested),
        ready: ready(),
        error: with_state(|state| state.error.clone()),
    }
}

/// Ask the portal for the hotkeys, then run the injection loop.
///
/// Runs in its own task on purpose: binding the shortcuts pops a consent
/// dialog, and launching a game must never wait for a human to click it.
/// A missing portal is a supported state — the caller gets no error, just no
/// hotkeys.
pub fn spawn(data_dir: &Path) -> tokio::task::JoinHandle<()> {
    let token_path = restore_token_path(data_dir);
    tokio::spawn(async move {
        match Hotkeys::register(token_path).await {
            Ok(hotkeys) => {
                let hotkeys = std::sync::Arc::new(hotkeys);
                // Published before the loop so `inject_now` can serve the RPCs as
                // soon as the shortcuts exist. `set` failing would mean another
                // task got here first, which `request_once` rules out.
                let _ = SERVICE.set(std::sync::Arc::clone(&hotkeys));
                hotkeys.run().await;
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
pub fn request_once(data_dir: &Path) -> bool {
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
        true
    });
    if !start {
        return false;
    }
    spawn(data_dir);
    true
}

/// Inject one action right now — this is what the `scale.*` RPCs and the GUI
/// buttons go through.
///
/// Without granted hotkeys there is no way to press the key (gamescope listens
/// to nothing else), so that case answers with [`PortalError::NotReady`] rather
/// than pretending to have changed anything.
pub async fn inject_now(action: GamescopeAction) -> Result<(), PortalError> {
    match SERVICE.get() {
        Some(hotkeys) => hotkeys.inject(action).await,
        None => Err(PortalError::NotReady),
    }
}

/// Has the portal granted the hotkeys, i.e. can the daemon inject at all?
pub fn ready() -> bool {
    SERVICE.get().is_some()
}

impl Hotkeys {
    /// Bind the shortcuts. This is the call that pops the portal's consent
    /// dialog listing every action below.
    async fn register(token_path: PathBuf) -> Result<Self, PortalError> {
        let shortcuts = GlobalShortcuts::new().await?;
        let session = shortcuts
            .create_session(CreateSessionOptions::default())
            .await?;

        let wanted: Vec<NewShortcut> = GamescopeAction::ALL
            .iter()
            .map(|action| {
                NewShortcut::new(action.id(), action.description())
                    .preferred_trigger(action.preferred_trigger())
            })
            .collect();

        let bound = shortcuts
            .bind_shortcuts(&session, &wanted, None, Default::default())
            .await?
            .response()?;

        let granted: Vec<&str> = bound.shortcuts().iter().map(|s| s.id()).collect();
        for action in GamescopeAction::ALL {
            if !granted.contains(&action.id()) {
                tracing::warn!(
                    "portal 没有授予热键 {}（{}）",
                    action.id(),
                    action.description()
                );
            }
        }
        tracing::info!("运行时缩放热键已注册：{}", granted.join(", "));

        let remote = RemoteDesktop::new().await?;
        Ok(Self {
            remote,
            shortcuts,
            desktop_session: Mutex::new(None),
            token_path,
        })
    }

    /// Wait for triggers and inject the matching chord.
    async fn run(self: std::sync::Arc<Self>) {
        let mut stream = match self.shortcuts.receive_activated().await {
            Ok(stream) => Box::pin(stream),
            Err(err) => {
                set_error(format!("无法监听热键触发：{err}"));
                tracing::warn!("无法监听热键触发：{err}");
                return;
            }
        };

        while let Some(activated) = stream.next().await {
            let Some(action) = GamescopeAction::from_id(activated.shortcut_id()) else {
                continue;
            };
            match self.inject(action).await {
                Ok(()) => tracing::info!(
                    "缩放热键 {} → 已注入 Super+{}",
                    action.id(),
                    action.keysym() as u8 as char
                ),
                // Deliberately not recorded as the service error: the hotkeys
                // themselves still work, only this press did not land.
                Err(err) => tracing::warn!("缩放热键 {} 注入失败：{err}", action.id()),
            }
        }
        set_error("portal 会话已关闭，缩放热键失效（重启守护进程可重新申请）".to_string());
        tracing::warn!("热键监听结束（portal 会话关闭）");
    }

    /// Inject one action, opening the injection session on first use.
    async fn inject(&self, action: GamescopeAction) -> Result<(), PortalError> {
        let mut guard = self.desktop_session.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_desktop_session().await?);
        }
        let Some(session) = guard.as_ref() else {
            return Ok(());
        };

        for (keysym, state) in action.sequence() {
            self.remote
                .notify_keyboard_keysym(session, keysym, state, Default::default())
                .await?;
            tokio::time::sleep(KEY_GAP).await;
        }
        Ok(())
    }

    /// Ask for keyboard injection rights. This is the second (and, with a
    /// stored token, the only other) consent dialog.
    async fn open_desktop_session(&self) -> Result<Session<RemoteDesktop>, PortalError> {
        let token = read_restore_token(&self.token_path);
        if token.is_some() {
            tracing::debug!("复用已有的 portal 授权令牌");
        }

        let session = self
            .remote
            .create_session(CreateSessionOptions::default())
            .await?;
        self.remote
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(BitFlags::from_flag(DeviceType::Keyboard))
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token.as_deref()),
            )
            .await?
            .response()?;
        let selected = self
            .remote
            .start(&session, None, StartOptions::default())
            .await?
            .response()?;

        if let Some(fresh) = selected.restore_token() {
            match write_restore_token(&self.token_path, fresh) {
                Ok(()) => tracing::info!("已保存 portal 授权令牌，下次不再询问"),
                Err(source) => {
                    // Not fatal: the feature still works, the user is just asked
                    // again next time.
                    tracing::warn!(
                        "授权令牌写不进去（{}）：下次启动还会问一次",
                        PortalError::Token {
                            path: self.token_path.clone(),
                            source,
                        }
                    );
                }
            }
        }
        tracing::info!("portal 键盘注入会话已建立");
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("kotori-hotkeys-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn every_action_round_trips_through_its_id() {
        for action in GamescopeAction::ALL {
            assert_eq!(GamescopeAction::from_id(action.id()), Some(action));
        }
        assert_eq!(GamescopeAction::from_id("nope"), None);
    }

    #[test]
    fn ids_and_triggers_are_unique() {
        let ids: Vec<&str> = GamescopeAction::ALL.iter().map(|a| a.id()).collect();
        let triggers: Vec<&str> = GamescopeAction::ALL
            .iter()
            .map(|a| a.preferred_trigger())
            .collect();
        let keysyms: Vec<i32> = GamescopeAction::ALL.iter().map(|a| a.keysym()).collect();
        for set in [&ids, &triggers] {
            let mut seen = set.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), set.len(), "重复项：{set:?}");
        }
        let mut seen_keysyms = keysyms.clone();
        seen_keysyms.sort_unstable();
        seen_keysyms.dedup();
        assert_eq!(seen_keysyms.len(), keysyms.len(), "重复按键：{keysyms:?}");
    }

    #[test]
    fn the_chord_holds_super_around_the_key() {
        let seq = GamescopeAction::ToggleFsr.sequence();
        assert_eq!(seq[0], (KEYSYM_SUPER_L, KeyState::Pressed));
        assert_eq!(seq[1], (0x75, KeyState::Pressed));
        assert_eq!(seq[2], (0x75, KeyState::Released));
        assert_eq!(seq[3], (KEYSYM_SUPER_L, KeyState::Released));

        // Keysym values are the lowercase letters gamescope watches for.
        assert_eq!(GamescopeAction::ToggleNis.keysym(), 'y' as i32);
        assert_eq!(GamescopeAction::ToggleNearest.keysym(), 'n' as i32);
        assert_eq!(GamescopeAction::ToggleLinear.keysym(), 'b' as i32);
        assert_eq!(GamescopeAction::ToggleFullscreen.keysym(), 'f' as i32);
    }

    #[test]
    fn sharper_means_gamescopes_softer_key() {
        // gamescope's help calls Super+I "increase FSR sharpness", but that key
        // raises a value RCAS reads as reduction-of-sharpness. The user asks for
        // a direction, so the mapping is inverted here — this test is the only
        // thing standing between that inversion and a silent regression.
        let sharper = GamescopeAction::for_sharpness_delta(1).unwrap();
        let softer = GamescopeAction::for_sharpness_delta(-1).unwrap();
        assert_eq!(sharper, GamescopeAction::Sharpen);
        assert_eq!(sharper.keysym(), 'o' as i32);
        assert_eq!(softer, GamescopeAction::Soften);
        assert_eq!(softer.keysym(), 'i' as i32);
        assert_eq!(GamescopeAction::for_sharpness_delta(0), None);

        // The descriptions must agree with the keys they press.
        assert!(GamescopeAction::Sharpen.description().contains("提高"));
        assert!(GamescopeAction::Soften.description().contains("降低"));
    }

    #[test]
    fn the_restore_token_is_stored_privately() {
        let path = restore_token_path(&temp_path().join("data"));
        assert!(read_restore_token(&path).is_none(), "还没写过就读到了东西");

        write_restore_token(&path, "token-123\n").unwrap();
        // Trailing newlines are ignored, otherwise a portal would reject it.
        assert_eq!(read_restore_token(&path).as_deref(), Some("token-123"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "授权令牌必须只有本人可读");
        }

        // An empty file means "no token" rather than an empty string token.
        std::fs::write(&path, "   ").unwrap();
        assert!(read_restore_token(&path).is_none());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn descriptions_name_the_chord_they_inject() {
        // The portal dialog is the only place the user learns the trigger, so a
        // description that drifts from the chord would be actively misleading.
        for action in GamescopeAction::ALL {
            assert!(
                action.description().contains(action.shortcut_hint()),
                "{} 的描述里没写快捷键：{}",
                action.id(),
                action.description()
            );
        }
    }

    #[tokio::test]
    async fn injection_without_granted_hotkeys_says_so() {
        // No test registers the portal service, so this is exactly the state a
        // fresh daemon is in: the RPCs must report it rather than claim success.
        assert!(!ready());
        let err = inject_now(GamescopeAction::ToggleFsr).await.unwrap_err();
        assert!(err.to_string().contains("还没就绪"), "{err}");

        // `daemon.status` builds its answer out of this, and "not requested yet"
        // has to be distinguishable from "requested and failed".
        let status = status();
        assert!(!status.ready);
        assert!(!status.requested, "没有测试调用过 request_once");
        assert!(status.error.is_none());
    }
}

//! Secret storage.
//!
//! **策略(2026-09-13 用户拍板,取代 ADR-014 的旧顺序)**:默认就是**明文凭据文件**
//! (权限 0600),密钥环是"有就用、没有不强求",主密码加密文件是留给想更严的人的选项。
//! 换句话说:**绝不再要求用户为了存个 B2 key 去输主密码或配置密钥环** —— 日常开发工具
//! (opencode 的 `auth.json`、gh 的 `hosts.yml`)都是这么做的,保护交给文件权限。
//!
//! 挑选顺序(`open_default`,从"最严"到"最省事"):
//!   1. **主密码加密文件**已存在 —— 那是用户的明确选择,而且只有他知道密码;
//!   2. **系统密钥环**在跑 —— 零输入,由操作系统保护;现存明文凭据顺手搬进去并删掉明文;
//!   3. **明文凭据文件** —— 默认落点,创建即 0600(见 [`plain`]);
//!   4. **内存** —— 只剩测试与"连文件都写不下去"的极端情况,进程一退就没了。
//!
//! 两条不变的红线:**绝不自研密码学**(只用 RustCrypto,见 [`encrypted`]);**绝不把明文
//! 悄悄写进 `config.toml`** —— 凭据只在上面这四个地方之一。
//!
//! **双系统规则**(用户在同一台机器上跑 Linux *和* Windows,共用同一个 bucket):
//!   * 同步密码**由用户自己选,我们绝不生成** —— 他没见过的密码会让备份只能靠这个程序打开;
//!   * 每个系统各存一份(Secret Service / Windows 凭据管理器),密钥环不跨系统同步;
//!   * Windows 上 `%APPDATA%` 的用户 ACL 就是明文文件的 0600,将来接上凭据管理器后更强。
//!
//! 只有 Linux 的密钥环后端(Secret Service,通过 `secret-tool`)实现了;Windows 的后端
//! 属于 Windows 移植的一部分(AGENTS.md),在那之前明文文件就是 Windows 的落点。

/// The master-password file backend, public so callers can ask about its
/// format version and password rules.
pub mod encrypted;
/// 明文凭据文件(默认落点)。
pub mod plain;

pub use encrypted::EncryptedFile;
pub use plain::PlainFile;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Attributes identifying kotori's entries in the keyring.
pub const SERVICE: &str = "kotori";
/// Environment override pointing at the `secret-tool` binary (used by tests).
pub const TOOL_ENV: &str = "KOTORI_SECRET_TOOL";
/// Account name used to probe whether a backend is actually listening. It is
/// deliberately not one of ours, so the lookup can never find anything.
const PROBE_ACCOUNT: &str = "__kotori_probe__";

/// One stored secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKey {
    /// Backblaze application key id.
    B2KeyId,
    /// Backblaze application key.
    B2AppKey,
    /// Save-sync password, in the clear, so the user can always read it back.
    SyncPassword,
    /// The same password in the form rclone wants, so a sync run never has to
    /// put it on a command line.
    SyncPasswordObscured,
}

impl SecretKey {
    pub const ALL: [SecretKey; 4] = [
        Self::B2KeyId,
        Self::B2AppKey,
        Self::SyncPassword,
        Self::SyncPasswordObscured,
    ];

    /// Keyring "account" attribute; also what the user types into `secret-tool`.
    pub fn account(self) -> &'static str {
        match self {
            Self::B2KeyId => "b2-key-id",
            Self::B2AppKey => "b2-app-key",
            Self::SyncPassword => "sync-password",
            Self::SyncPasswordObscured => "sync-password-obscured",
        }
    }

    /// Human-readable label shown by keyring managers.
    pub fn label(self) -> &'static str {
        match self {
            Self::B2KeyId => "kotori: B2 key id",
            Self::B2AppKey => "kotori: B2 application key",
            Self::SyncPassword => "kotori: sync password",
            Self::SyncPasswordObscured => "kotori: sync password (rclone form)",
        }
    }
}

/// What to do about a missing keyring, **in the terms of the system the user is
/// actually on**.
///
/// Kept in exactly one place: the first version of this message told everyone
/// to start `kwalletd6` and add it to their niri config, which is nonsense on a
/// GNOME box, inside a container, and on Windows. Platform advice belongs next
/// to the other platform switches (see AGENTS.md ADR-011).
pub const fn keyring_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "桌面环境（KDE / GNOME 等）通常已经替你启动了密钥环；只有窗管、没有桌面的会话\
         （niri、sway、纯 TTY、容器）不会，需要在会话启动时拉起一个 Secret Service 提供者，\
         例如 gnome-keyring-daemon --start --components=secrets，或 KWallet 的 kwalletd6。"
    } else if cfg!(windows) {
        "Windows 上应该使用系统凭据管理器（它总是可用）；看到这条说明凭据后端还没接上。"
    } else {
        "这个系统上还没有可用的密钥环后端。"
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("找不到 secret-tool，无法访问系统密钥环。{hint}", hint = keyring_hint())]
    BackendMissing,
    /// `secret-tool` exists but nothing is answering on D-Bus.
    ///
    /// This is the normal state on a session that never started a Secret
    /// Service provider (Niri, Sway, a bare TTY) — being installed is not the
    /// same as being *running*, and treating the two as one is what makes a
    /// settings page claim everything is fine while every save fails.
    #[error("系统密钥环没有在运行：{detail}\n{hint}", hint = keyring_hint())]
    BackendNotRunning { detail: String },
    #[error("这个平台的密钥环后端还没实现（{0}）；密码目前只能保存在内存里")]
    BackendUnsupported(&'static str),
    /// The master password did not open the file. Indistinguishable from a
    /// corrupt file, and overwhelmingly the more likely of the two.
    #[error("主密码不对（或者凭据文件损坏了）")]
    WrongMasterPassword,
    #[error("凭据文件已锁定，需要先用主密码解锁")]
    Locked,
    #[error("主密码至少要 {minimum} 个字符")]
    MasterPasswordTooShort { minimum: usize },
    #[error("加解密失败: {0}")]
    Crypto(String),
    #[error("凭据文件读写失败: {0}")]
    Io(String),
    #[error("密钥环操作失败: {0}")]
    Command(String),
}

impl SecretError {
    /// `secret-tool` reports both "no such entry" and "no backend" by exiting
    /// non-zero; only the second one writes to stderr, which is the only thing
    /// that tells them apart.
    fn from_stderr(stderr: &str) -> Self {
        let stderr = stderr.trim();
        if stderr.is_empty() {
            // Unreachable in practice, but never claim a broken backend for a
            // silent failure.
            return Self::Command("secret-tool 失败但没有输出".to_string());
        }
        Self::BackendNotRunning {
            detail: stderr.to_string(),
        }
    }
}

/// Which keyring implementation this build talks to.
pub const fn backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "Secret Service (libsecret)"
    } else if cfg!(windows) {
        "Windows 凭据管理器（尚未实现）"
    } else {
        "未支持的系统密钥环"
    }
}

/// Where a [`Keyring`] keeps its entries.
#[derive(Debug, Clone)]
enum Backend {
    /// The platform's own store, driven through `secret-tool`.
    Tool(PathBuf),
    /// 明文文件(权限 0600)—— **默认落点**(见 [`plain`])。
    PlainFile(PlainFile),
    /// A file sealed with a master password the user chose; the opt-in
    /// "stricter" tier (see [`encrypted`]).
    EncryptedFile(EncryptedFile),
    /// Nothing to persist to: hold the secrets for this session only.
    Memory(Arc<Mutex<HashMap<SecretKey, String>>>),
}

/// Which store is in use, in a form clients can display.
///
/// "Locked" is deliberately its own state: a locked file is not the same as an
/// empty one, and conflating them is what sends users off to re-enter
/// credentials that were never the problem.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StoreKind {
    /// The platform keyring.
    System { backend: String },
    /// 明文凭据文件(0600)。
    PlainFile { path: String },
    /// A master-password file on disk.
    EncryptedFile { path: String, locked: bool },
    /// Session only: nothing survives a restart.
    SessionOnly,
}

/// Name of the platform store, for [`StoreKind::System`].
fn describe_backend(keyring: &Keyring) -> &'static str {
    match &keyring.backend {
        Backend::Tool(_) => backend_name(),
        Backend::PlainFile(_) => "明文凭据文件（权限 0600）",
        Backend::EncryptedFile(_) => "主密码加密文件",
        Backend::Memory(_) => "内存（仅本次会话）",
    }
}

/// Handle to a secret store.
///
/// Normally this is the plaintext file in the config directory (0600) — see
/// [`Keyring::open_default`] for the exact order. The OS keyring and the
/// master-password file are both **optional** tiers on top of that.
#[derive(Debug, Clone)]
pub struct Keyring {
    backend: Backend,
}

/// 把明文文件里的凭据搬进刚可用的密钥环,搬全了才删明文。
///
/// 为什么要有这一步:用户可能在 niri(没有密钥环)下先用明文存过凭据,之后又回到 Plasma。
/// 如果不搬,密钥环一接管,那些凭据就像"消失"了一样,用户会以为丢了而重填一遍。
/// 搬不全是**保留明文文件** —— 宁可留一份明文,也不能让凭据凭空少一条。
fn adopt_plain_entries(keyring: &Keyring, plain_path: &Path) {
    let plain = PlainFile::new(plain_path);
    if !plain.exists() {
        return;
    }
    let entries = match plain.load() {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!("明文凭据文件读不出来（{error}），先留着它，不搬到密钥环");
            return;
        }
    };
    if entries.is_empty() {
        if plain.remove().is_ok() {
            tracing::info!("删掉空的明文凭据文件 {}", plain_path.display());
        }
        return;
    }
    let mut moved = 0;
    for (key, value) in &entries {
        match keyring.set(*key, value) {
            Ok(()) => moved += 1,
            Err(error) => tracing::warn!("{} 搬进密钥环失败: {error}", key.account()),
        }
    }
    if moved == entries.len() && plain.remove().is_ok() {
        tracing::info!(
            "系统密钥环可用了，{moved} 条明文凭据已搬进去，明文文件已删除（{}）",
            plain_path.display()
        );
    } else {
        tracing::warn!(
            "只搬了 {moved}/{} 条到密钥环，明文文件保留在 {}",
            entries.len(),
            plain_path.display()
        );
    }
}

impl Keyring {
    /// Use a specific `secret-tool` binary.
    pub fn with_tool(tool: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::Tool(tool.into()),
        }
    }

    /// A store that lives only as long as this process.
    pub fn memory() -> Self {
        Self {
            backend: Backend::Memory(Arc::new(Mutex::new(HashMap::new()))),
        }
    }

    /// 明文凭据文件(权限 0600)。文件不存在就是"还没存过",写的时候才建。
    pub fn plain_file(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::PlainFile(PlainFile::new(path)),
        }
    }

    /// A master-password file. It may be locked; unlocking is separate.
    ///
    /// 只有测试还需要直接造一个加密后端(生产路径走 `open_default` 或
    /// `from_encrypted`),所以这里按测试构建裁剪掉。
    #[cfg(test)]
    pub fn encrypted_file(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::EncryptedFile(EncryptedFile::new(path)),
        }
    }

    /// Wrap a file store that has already been opened (and possibly unlocked).
    ///
    /// Handles are cheap and share their unlock state, so this is how a caller
    /// keeps a store it just unlocked instead of starting over — which would
    /// mean deriving the key a second time and, worse, appearing locked again.
    pub fn from_encrypted(file: EncryptedFile) -> Self {
        Self {
            backend: Backend::EncryptedFile(file),
        }
    }

    /// Pick the store this machine should use. The order is the whole policy:
    ///
    /// 1. **主密码加密文件**(存在就用)—— 用户的明确选择,只有他知道密码;
    /// 2. **系统密钥环**(在跑就用)—— 零输入;明文里已有的凭据顺手搬进去并删掉明文,
    ///    能不留明文就不留;
    /// 3. **明文凭据文件**(默认)—— 零输入、跨重启,权限 0600;
    /// 4. **内存** —— 只有"连文件都写不下去"时才轮到这里(正常路径到不了)。
    ///
    /// 第二个返回值现在恒为 `false`:`open_default` 不会再落到"什么都存不住"的状态,
    /// 参数保留是为了兼容调用方与将来可能出现的只读配置目录。
    pub fn open_default(encrypted_path: &Path, plain_path: &Path) -> (Self, bool) {
        Self::open_default_with(Self::system(), encrypted_path, plain_path)
    }

    /// [`open_default`] with the keyring probe passed in.
    ///
    /// Split out so the *policy* — which store wins — can be tested without the
    /// machine the test happens to run on deciding the answer. That is not
    /// hypothetical: the fallback test passed on a niri session (no Secret
    /// Service provider running) and failed on a Plasma one (ksecretd is up), and
    /// neither result said anything about the policy.
    fn open_default_with(
        keyring: Result<Self, SecretError>,
        encrypted_path: &Path,
        plain_path: &Path,
    ) -> (Self, bool) {
        let encrypted = EncryptedFile::new(encrypted_path);
        if encrypted.exists() {
            return (Self::from_encrypted(encrypted), false);
        }

        if let Ok(keyring) = keyring {
            adopt_plain_entries(&keyring, plain_path);
            return (keyring, false);
        }

        tracing::info!(
            "没有运行中的系统密钥环，凭据存到明文文件 {}（权限 0600）",
            plain_path.display()
        );
        (Self::plain_file(plain_path), false)
    }

    /// The encrypted-file backend, if that is what this handle is.
    pub fn encrypted_store(&self) -> Option<&EncryptedFile> {
        match &self.backend {
            Backend::EncryptedFile(file) => Some(file),
            _ => None,
        }
    }

    /// Which store is in use, for the settings page.
    pub fn kind(&self) -> StoreKind {
        match &self.backend {
            Backend::Tool(_) => StoreKind::System {
                backend: describe_backend(self).to_string(),
            },
            Backend::PlainFile(file) => StoreKind::PlainFile {
                path: file.path().display().to_string(),
            },
            Backend::EncryptedFile(file) => StoreKind::EncryptedFile {
                path: file.path().display().to_string(),
                locked: !file.is_unlocked(),
            },
            Backend::Memory(_) => StoreKind::SessionOnly,
        }
    }

    /// Detect the system keyring, or explain what is missing.
    ///
    /// On Windows the Credential Manager backend still has to be written; until
    /// then the caller falls back to the plaintext file (0600, or `%APPDATA%`'s
    /// per-user ACL), which is what every other tool on that platform does.
    pub fn system() -> Result<Self, SecretError> {
        if !cfg!(target_os = "linux") {
            return Err(SecretError::BackendUnsupported(backend_name()));
        }

        // The explicit override wins, but it must go through the same probe:
        // an override that points at a present-but-dead tool is exactly the
        // situation this check exists for.
        let tool = std::env::var_os(TOOL_ENV)
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| crate::util::executor::find_binary("secret-tool"))
            .ok_or(SecretError::BackendMissing)?;

        let keyring = Self::with_tool(tool);
        // Being installed is not being available: probe before promising
        // anything to the caller.
        keyring.probe()?;
        Ok(keyring)
    }

    /// Ask the backend a question it can always answer.
    ///
    /// A lookup for an entry that cannot exist is the cheapest real round trip:
    /// it fails with empty stderr when the store is healthy (the entry is
    /// simply absent) and with the D-Bus error when nothing is listening.
    pub fn probe(&self) -> Result<(), SecretError> {
        match &self.backend {
            // Nothing to be missing.
            Backend::Memory(_) => return Ok(()),
            // 明文文件:后端就是文件本身,没有"在不在跑"这回事。
            Backend::PlainFile(_) => return Ok(()),
            // The file is the backend; whether it is *unlocked* is a separate
            // question that `kind()` answers.
            Backend::EncryptedFile(file) => {
                return if file.exists() || file.is_unlocked() {
                    Ok(())
                } else {
                    Err(SecretError::Locked)
                };
            }
            Backend::Tool(_) => {}
        }
        let output = self.run(
            &["lookup", "service", SERVICE, "account", PROBE_ACCOUNT],
            None,
        )?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Ok(())
        } else {
            Err(SecretError::from_stderr(&stderr))
        }
    }

    /// True when secrets are held in memory and lost on restart.
    pub fn is_ephemeral(&self) -> bool {
        matches!(self.backend, Backend::Memory(_))
    }

    /// 这一级存储下,"怎么自己把密码取回来"的诚实答案。
    ///
    /// 三级存储的取回方式完全不同:密钥环能用 `secret-tool` 直接读,凭据文件只有
    /// 主密码(所以我们干脆说明没有第二条路),内存那一级重启就没了。**说错比不说更坏** ——
    /// 用户会照着一条跑不通的命令去找一个根本不在那儿的密码。
    pub fn lookup_hint(&self, key: SecretKey) -> String {
        match &self.backend {
            Backend::Tool(_) => lookup_hint(key),
            Backend::PlainFile(file) => format!(
                "凭据就明文写在 {} 里（权限 0600，只有你能读）—— 你自己打开就能看见。",
                file.path().display()
            ),
            Backend::EncryptedFile(file) => format!(
                "凭据在你自己设的主密码文件 {} 里,只有主密码能打开它 —— 忘了就只能删掉重设。",
                file.path().display()
            ),
            Backend::Memory(_) => {
                "本次会话没有可持久化的后端,守护进程一停密码就没了,取不回来。".to_string()
            }
        }
    }

    /// Name of the store, for the settings page.
    pub fn describe(&self) -> String {
        match &self.backend {
            Backend::Tool(tool) => format!("{} ({})", backend_name(), tool.display()),
            Backend::PlainFile(file) => format!(
                "明文凭据文件 {}（权限 0600，只有你能读）",
                file.path().display()
            ),
            Backend::EncryptedFile(file) => format!(
                "主密码加密文件 {}（{}）",
                file.path().display(),
                if file.is_unlocked() {
                    "已解锁"
                } else {
                    "已锁定，需要主密码"
                }
            ),
            Backend::Memory(_) => "内存（连凭据文件都写不下去：检查配置目录的写权限）".to_string(),
        }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<std::process::Output, SecretError> {
        let Backend::Tool(tool) = &self.backend else {
            return Err(SecretError::Command("这个存储不需要外部命令".to_string()));
        };
        let mut command = Command::new(tool);
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|e| SecretError::Command(format!("{}: {e}", tool.display())))?;

        if let Some(value) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| SecretError::Command("无法写入 secret-tool 标准输入".to_string()))?;
            pipe.write_all(value.as_bytes())
                .and_then(|_| pipe.write_all(b"\n"))
                .map_err(|e| SecretError::Command(e.to_string()))?;
        }

        child
            .wait_with_output()
            .map_err(|e| SecretError::Command(e.to_string()))
    }

    /// Read a secret. `Ok(None)` means "not stored".
    pub fn get(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                return Ok(store.get(&key).cloned());
            }
            Backend::PlainFile(file) => {
                return Ok(file
                    .load()?
                    .into_iter()
                    .find(|(stored, _)| *stored == key)
                    .map(|(_, value)| value));
            }
            Backend::EncryptedFile(file) => {
                // A locked file is an error, not an empty result: the caller
                // has to be able to tell "unlock me" from "nothing here".
                return Ok(file
                    .load()?
                    .into_iter()
                    .find(|(stored, _)| *stored == key)
                    .map(|(_, value)| value));
            }
            Backend::Tool(_) => {}
        }

        let output = self.run(
            &["lookup", "service", SERVICE, "account", key.account()],
            None,
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                // secret-tool exits non-zero when the entry does not exist.
                return Ok(None);
            }
            // A broken backend must not look like "nothing stored": that sends
            // the user off to re-enter credentials that were never the problem.
            return Err(SecretError::from_stderr(&stderr));
        }
        let value = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        Ok(if value.is_empty() { None } else { Some(value) })
    }

    /// Store a secret, replacing any previous value.
    ///
    /// The value goes in over stdin so it never appears in `ps`.
    pub fn set(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let mut store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                store.insert(key, value.to_string());
                return Ok(());
            }
            Backend::PlainFile(file) => {
                let mut entries = file.load()?;
                match entries.iter_mut().find(|(stored, _)| *stored == key) {
                    Some(slot) => slot.1 = value.to_string(),
                    None => entries.push((key, value.to_string())),
                }
                return file.store(&entries);
            }
            Backend::EncryptedFile(file) => {
                let mut entries = file.load()?;
                match entries.iter_mut().find(|(stored, _)| *stored == key) {
                    Some(slot) => slot.1 = value.to_string(),
                    None => entries.push((key, value.to_string())),
                }
                return file.store(&entries);
            }
            Backend::Tool(_) => {}
        }

        let label = format!("--label={}", key.label());
        let output = self.run(
            &[
                "store",
                &label,
                "service",
                SERVICE,
                "account",
                key.account(),
            ],
            Some(value),
        )?;
        if !output.status.success() {
            return Err(SecretError::from_stderr(&String::from_utf8_lossy(
                &output.stderr,
            )));
        }
        Ok(())
    }

    /// Remove a secret. Succeeds when it was not there in the first place.
    pub fn clear(&self, key: SecretKey) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let mut store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                store.remove(&key);
                return Ok(());
            }
            Backend::PlainFile(file) => {
                let mut entries = file.load()?;
                entries.retain(|(stored, _)| *stored != key);
                return file.store(&entries);
            }
            Backend::EncryptedFile(file) => {
                let mut entries = file.load()?;
                entries.retain(|(stored, _)| *stored != key);
                return file.store(&entries);
            }
            Backend::Tool(_) => {}
        }

        let output = self.run(
            &["clear", "service", SERVICE, "account", key.account()],
            None,
        )?;
        if !output.status.success() {
            return Err(SecretError::Command(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }

    /// Everything currently held in the session store.
    ///
    /// Used to move credentials into the real keyring if one becomes available
    /// later, so a user who answered the prompt while no keyring was running
    /// does not have to type them again.
    pub fn snapshot(&self) -> Vec<(SecretKey, String)> {
        if let Backend::PlainFile(file) = &self.backend {
            // 明文文件里的东西要能被搬到别处(例如用户改成主密码加密)。
            return file.load().unwrap_or_default();
        }
        if let Backend::EncryptedFile(file) = &self.backend {
            // Locked or unreadable: nothing to hand over, and an unlocked file
            // is already persistent so it never needs migrating.
            return file.load().unwrap_or_default();
        }
        let Backend::Memory(store) = &self.backend else {
            return Vec::new();
        };
        let Ok(store) = store.lock() else {
            return Vec::new();
        };
        SecretKey::ALL
            .into_iter()
            .filter_map(|key| store.get(&key).map(|value| (key, value.clone())))
            .collect()
    }

    /// Which of our secrets exist. Used by the settings page.
    pub fn present(&self) -> Vec<SecretKey> {
        SecretKey::ALL
            .into_iter()
            .filter(|key| matches!(self.get(*key), Ok(Some(_))))
            .collect()
    }
}

/// The exact command a user can run to read a secret without kotori.
///
/// Kept per-platform on purpose: "how do I get my password back" must have a
/// concrete answer on every system the user boots into.
pub fn lookup_hint(key: SecretKey) -> String {
    if cfg!(target_os = "linux") {
        format!(
            "secret-tool lookup service {SERVICE} account {}",
            key.account()
        )
    } else if cfg!(windows) {
        // Do NOT send the user to the Credential Manager: the Windows backend
        // is not written yet, so there is no entry there to find. Promising a
        // retrieval path we never created is worse than admitting there is none.
        "（Windows 端的凭据管理器后端尚未实现，密码目前只存在于内存中，\
         重启后需要重新设置；实现之后会在这里给出查阅方式）"
            .to_string()
    } else {
        format!("（{} 上还没有查阅方式）", backend_name())
    }
}

/// A stand-in `secret-tool` backed by a file, so tests never touch the user's
/// real keyring. Shared by the sync tests, which need one too.
#[cfg(test)]
pub(crate) mod testing {
    use super::{Keyring, SecretKey};
    use std::path::{Path, PathBuf};

    /// Argument every fake helper answers with an immediate, side-effect-free
    /// exit. Used by [`write_executable`].
    pub(crate) const WARMUP_FLAG: &str = "--kotori-warmup";

    /// Write a helper script and prove the kernel will actually run it.
    ///
    /// Writing a file and exec'ing it are each safe on their own, but tests run
    /// in parallel: another thread can fork between our write and our first
    /// exec and inherit the still-open write handle, which makes the kernel
    /// report `ETXTBSY` for that inode until the child execs. Retrying
    /// converges, because once our own write handle is closed nothing can open
    /// the file for writing again — and every fake script exits immediately on
    /// [`WARMUP_FLAG`], so the warm-up has no side effects.
    pub(crate) fn write_executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();

        for _ in 0..200 {
            match std::process::Command::new(path).arg(WARMUP_FLAG).output() {
                Ok(_) => return,
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("cannot execute {}: {e}", path.display()),
            }
        }
        panic!("{} stayed busy", path.display());
    }

    pub(crate) struct FakeTool {
        dir: PathBuf,
        tool: PathBuf,
    }

    impl FakeTool {
        pub(crate) fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-secrets-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let tool = dir.join("secret-tool");

            // Mirrors the parts of secret-tool's interface kotori relies on:
            // entries are keyed by their `account` attribute, `store` reads the
            // secret from stdin, `lookup` prints it, `clear` removes it.
            let script = format!(
                r#"#!/bin/sh
[ "$1" = "{WARMUP_FLAG}" ] && exit 0
dir='{dir}'
name=''
prev=''
for a in "$@"; do
  if [ "$prev" = "account" ]; then name="$a"; fi
  prev="$a"
done
file="$dir/$name"
case "$1" in
  store) read -r v; printf '%s' "$v" > "$file" ;;
  lookup) [ -s "$file" ] && cat "$file" || exit 1 ;;
  clear) rm -f "$file" ;;
  *) exit 2 ;;
esac
"#,
                dir = dir.display()
            );
            write_executable(&tool, &script);

            Self { dir, tool }
        }

        pub(crate) fn keyring(&self) -> Keyring {
            Keyring::with_tool(&self.tool)
        }

        /// A `secret-tool` that exists but has no backend behind it — exactly
        /// what a session that never started a Secret Service provider looks
        /// like (the binary is installed, the D-Bus name answer is not).
        pub(crate) fn broken(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-secrets-broken-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let tool = dir.join("secret-tool");
            write_executable(
                &tool,
                "#!/bin/sh\n\
                 [ \"$1\" = \"--kotori-warmup\" ] && exit 0\n\
                 echo 'secret-tool: The name is not activatable' >&2\nexit 1\n",
            );
            Self { dir, tool }
        }

        /// Where the fake tool keeps one account's secret.
        pub(crate) fn stored(&self, key: SecretKey) -> PathBuf {
            self.dir.join(key.account())
        }
    }

    impl Drop for FakeTool {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeTool;
    use super::*;

    #[test]
    fn round_trips_a_secret_through_the_keyring() {
        let fake = FakeTool::new("roundtrip");
        let keyring = fake.keyring();

        assert_eq!(keyring.get(SecretKey::SyncPassword).unwrap(), None);

        keyring
            .set(SecretKey::SyncPassword, "hunter2 with spaces")
            .unwrap();
        assert_eq!(
            keyring.get(SecretKey::SyncPassword).unwrap(),
            Some("hunter2 with spaces".to_string()),
            "values are passed verbatim, whitespace included"
        );

        // The stored value is what the user would see with plain secret-tool.
        assert_eq!(
            std::fs::read_to_string(fake.stored(SecretKey::SyncPassword)).unwrap(),
            "hunter2 with spaces"
        );

        keyring.clear(SecretKey::SyncPassword).unwrap();
        assert_eq!(keyring.get(SecretKey::SyncPassword).unwrap(), None);
    }

    #[test]
    fn secrets_are_stored_under_distinct_accounts() {
        let fake = FakeTool::new("distinct");
        let keyring = fake.keyring();
        keyring.set(SecretKey::B2KeyId, "keyid").unwrap();
        keyring
            .set(SecretKey::SyncPasswordObscured, "obscured-blob")
            .unwrap();

        assert_eq!(
            keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("keyid")
        );
        assert_eq!(
            keyring
                .get(SecretKey::SyncPasswordObscured)
                .unwrap()
                .as_deref(),
            Some("obscured-blob")
        );
        assert_eq!(keyring.get(SecretKey::B2AppKey).unwrap(), None);

        // The two password entries are separate items, so the readable one and
        // the one rclone consumes cannot clobber each other.
        assert_ne!(
            SecretKey::SyncPassword.account(),
            SecretKey::SyncPasswordObscured.account()
        );
    }

    #[test]
    fn an_installed_but_dead_backend_is_not_mistaken_for_a_working_one() {
        // The bug this covers: `secret-tool` was present, so the settings page
        // said the keyring was fine, while every store silently failed and
        // every read looked like "nothing saved yet".
        let fake = FakeTool::broken("dead");
        let keyring = fake.keyring();

        let error = keyring.probe().unwrap_err();
        assert!(
            matches!(error, SecretError::BackendNotRunning { .. }),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("没有在运行"), "{message}");
        // It has to say what to do about it, not just that it is broken...
        assert!(message.contains("密钥环"), "{message}");
        // ...and say it in the terms of *this* system. Advice for another
        // platform is worse than useless: it sends the user chasing a service
        // that does not exist there.
        #[cfg(target_os = "linux")]
        assert!(
            !message.contains("凭据管理器"),
            "Windows 的建议不该出现在 Linux 上: {message}"
        );
        #[cfg(windows)]
        {
            assert!(!message.contains("secret-tool"), "{message}");
            assert!(!message.contains("kwalletd6"), "{message}");
        }

        // A read must not masquerade as "nothing stored".
        let error = keyring.get(SecretKey::B2KeyId).unwrap_err();
        assert!(
            matches!(error, SecretError::BackendNotRunning { .. }),
            "{error:?}"
        );

        // And a write reports the real reason too.
        let error = keyring.set(SecretKey::B2AppKey, "x").unwrap_err();
        assert!(
            matches!(error, SecretError::BackendNotRunning { .. }),
            "{error:?}"
        );

        // `system_or_memory` is what the daemon uses, so it must degrade to a
        // session store rather than refusing to run at all.
        let memory = Keyring::memory();
        assert!(memory.is_ephemeral() && memory.probe().is_ok());
    }

    #[test]
    fn a_healthy_backend_answers_the_probe_without_finding_anything() {
        let fake = FakeTool::new("probe-ok");
        let keyring = fake.keyring();
        assert!(keyring.probe().is_ok(), "the entry simply does not exist");
        assert!(
            keyring.get(SecretKey::SyncPassword).unwrap().is_none(),
            "a healthy store still reports a missing entry as None"
        );
    }

    #[test]
    fn missing_backend_is_reported_instead_of_falling_back_to_plaintext() {
        let keyring = Keyring::with_tool("/nonexistent/secret-tool");
        let error = keyring.get(SecretKey::SyncPassword).unwrap_err();
        assert!(matches!(error, SecretError::Command(_)), "{error:?}");
    }

    #[test]
    fn lookup_hint_names_the_exact_command() {
        let hint = lookup_hint(SecretKey::SyncPassword);
        assert_eq!(
            hint,
            "secret-tool lookup service kotori account sync-password"
        );
        assert!(hint.contains(SERVICE));
    }

    #[test]
    fn system_detection_reports_what_is_on_this_machine() {
        // The real answer depends on the machine; the contract is that it either
        // yields a usable handle or a clear "no keyring here" error.
        match Keyring::system() {
            Ok(keyring) => assert!(!keyring.is_ephemeral()),
            Err(error) => {
                // Any of the three "not usable here" states is a valid answer
                // depending on the machine.
                assert!(
                    matches!(
                        error,
                        SecretError::BackendMissing
                            | SecretError::BackendUnsupported(_)
                            | SecretError::BackendNotRunning { .. }
                    ),
                    "{error:?}"
                );
            }
        }
    }

    /// **策略(2026-09-13)**:没有密钥环时落点是**明文 0600 文件**,不是内存。
    ///
    /// 这条测试以前断言的是反过来的东西("绝不退化成明文")—— 用户明确改了这个决定:
    /// 日常工具(opencode / gh)都是明文 0600,不该为了一个 B2 key 逼用户输主密码。
    /// 现在硬性的部分变成:**必须能持久化,而且必须是 0600**。
    ///
    /// "没有密钥环"是**注入**的,不是指望这台机器恰好没有:Plasma 会话里
    /// `ksecretd` 是被桌面拉起来的,KDE 上跑就会走到密钥环那一支(那是对的行为,
    /// 不是这条测试要断言的东西)。见 [`Keyring::open_default_with`]。
    #[test]
    fn a_machine_without_a_keyring_falls_back_to_a_private_plain_file() {
        let dir = std::env::temp_dir().join(format!(
            "kotori-plain-default-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let encrypted = dir.join("secrets.json");
        let plain = dir.join("credentials.json");

        let (store, ephemeral) =
            Keyring::open_default_with(Err(SecretError::BackendMissing), &encrypted, &plain);
        assert!(!ephemeral, "明文文件是能持久化的,不该报成临时的");
        assert!(!store.is_ephemeral());
        assert_eq!(
            store.kind(),
            StoreKind::PlainFile {
                path: plain.display().to_string()
            }
        );

        store.set(SecretKey::B2KeyId, "005keyid").unwrap();
        assert!(plain.is_file(), "写下去就该有文件");
        assert_eq!(
            store.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("005keyid")
        );
        // 换一个句柄重开(等价于守护进程重启),凭据要还在。
        let (reopened, _) =
            Keyring::open_default_with(Err(SecretError::BackendMissing), &encrypted, &plain);
        assert_eq!(
            reopened.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("005keyid")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&plain).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "明文文件必须 0600，实际 {mode:o}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 有密钥环时:明文里已有的凭据要**搬进去**,搬全了就把明文删掉 —— 能不留明文就不留。
    /// (用户 2026-09-13:"密钥环作为可选使用"。)
    #[test]
    fn a_keyring_that_shows_up_takes_over_the_plaintext_file() {
        let fake = FakeTool::new("takeover");
        let keyring = fake.keyring();
        let dir = std::env::temp_dir().join(format!(
            "kotori-takeover-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("credentials.json");
        let file = PlainFile::new(&plain);
        file.store(&[(SecretKey::B2KeyId, "005keyid".to_string())])
            .unwrap();

        adopt_plain_entries(&keyring, &plain);

        assert_eq!(
            keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("005keyid"),
            "明文里的凭据必须搬进密钥环,不能像凭空消失了一样"
        );
        assert!(!plain.exists(), "搬全了就该删掉明文");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 挑选顺序本身:密钥环在跑时它赢,明文里的东西被搬走后不再留明文。
    /// (顺序的另外两档——加密文件优先、都没有则明文——由上面两条测试覆盖。)
    #[test]
    fn a_running_keyring_wins_over_the_plaintext_file() {
        let fake = FakeTool::new("wins");
        let dir = std::env::temp_dir().join(format!(
            "kotori-wins-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let encrypted = dir.join("secrets.json");
        let plain = dir.join("credentials.json");
        PlainFile::new(&plain)
            .store(&[(SecretKey::B2KeyId, "005keyid".to_string())])
            .unwrap();

        let (store, ephemeral) = Keyring::open_default_with(Ok(fake.keyring()), &encrypted, &plain);

        assert!(!ephemeral);
        assert!(
            matches!(store.kind(), StoreKind::System { .. }),
            "{:?}",
            store.kind()
        );
        assert_eq!(
            store.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("005keyid"),
            "切到密钥环不能把已有凭据弄丢"
        );
        assert!(!plain.exists(), "搬全了就该删掉明文");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 加密文件是用户显式选过的更严那一级,它比密钥环更优先。
    #[test]
    fn an_encrypted_file_wins_over_a_running_keyring() {
        let fake = FakeTool::new("encrypted-first");
        let dir = std::env::temp_dir().join(format!(
            "kotori-encfirst-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let encrypted = dir.join("secrets.json");
        let plain = dir.join("credentials.json");
        // 一个存在(锁着)的加密文件就够:选它不需要密码,解锁是后面的事。
        EncryptedFile::new(&encrypted)
            .create(
                "a-password-long-enough",
                &[(SecretKey::B2KeyId, "005keyid".to_string())],
            )
            .unwrap();

        let (store, _) = Keyring::open_default_with(Ok(fake.keyring()), &encrypted, &plain);

        assert!(
            matches!(store.kind(), StoreKind::EncryptedFile { .. }),
            "{:?}",
            store.kind()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_machine_without_a_keyring_keeps_secrets_in_memory_only() {
        let keyring = Keyring::memory();
        assert!(keyring.is_ephemeral());
        assert!(keyring.get(SecretKey::SyncPassword).unwrap().is_none());

        keyring.set(SecretKey::SyncPassword, "hunter2").unwrap();
        assert_eq!(
            keyring.get(SecretKey::SyncPassword).unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(keyring.present(), vec![SecretKey::SyncPassword]);
        // The memory store never shells out, so it cannot leak through a file.
        assert!(keyring.run(&["lookup"], None).is_err());

        keyring.clear(SecretKey::SyncPassword).unwrap();
        assert!(keyring.present().is_empty());
        // Clearing twice is not an error, exactly like the real keyring.
        keyring.clear(SecretKey::SyncPassword).unwrap();
    }

    #[test]
    fn the_store_describes_itself_honestly() {
        assert!(Keyring::memory().describe().contains("内存"));
        let keyring = Keyring::with_tool("/usr/bin/secret-tool");
        assert!(keyring.describe().contains("/usr/bin/secret-tool"));
        assert!(!keyring.describe().contains("内存"));
    }

    #[test]
    fn the_retrieval_hint_only_promises_what_we_actually_implemented() {
        // Requirement: the user must be able to recover the password without
        // kotori — but only on systems where we actually store it somewhere
        // they can reach. Telling a Windows user to open the Credential
        // Manager would send them looking for an entry we never wrote.
        let hint = lookup_hint(SecretKey::SyncPassword);
        assert!(!hint.is_empty());
        assert_ne!(backend_name(), "");

        #[cfg(target_os = "linux")]
        assert_eq!(
            hint,
            "secret-tool lookup service kotori account sync-password"
        );

        #[cfg(windows)]
        {
            assert!(!hint.contains("凭据管理器 → "), "{hint}");
            assert!(hint.contains("尚未实现"), "{hint}");
        }

        // Whatever a platform says, it must not be empty advice.
        assert!(!keyring_hint().is_empty(), "{hint}");
    }

    /// 三级存储下"怎么取回密码"的答案不一样 —— 没有密钥环的机器上再指一条
    /// `secret-tool` 命令,就是让用户去找一个不在那儿的东西。
    #[test]
    fn the_retrieval_hint_follows_the_store_that_is_actually_in_use() {
        // 密钥环:给出能直接跑的命令。
        let fake = FakeTool::new("hint");
        let keyring = fake.keyring();
        assert!(
            keyring
                .lookup_hint(SecretKey::SyncPassword)
                .contains("secret-tool lookup"),
            "{:?}",
            keyring.lookup_hint(SecretKey::SyncPassword)
        );

        // 内存那一级:明说取不回来,不再提 secret-tool。
        let memory = Keyring::memory();
        let hint = memory.lookup_hint(SecretKey::SyncPassword);
        assert!(!hint.contains("secret-tool"), "{hint}");
        assert!(hint.contains("取不回来"), "{hint}");

        // 我们自己的方案:只有主密码,别再指去密钥环。
        let file = Keyring::encrypted_file("/tmp/kotori-hint/secrets.json");
        let hint = file.lookup_hint(SecretKey::SyncPassword);
        assert!(!hint.contains("secret-tool"), "{hint}");
        assert!(hint.contains("/tmp/kotori-hint/secrets.json"), "{hint}");
        assert!(hint.contains("主密码"), "{hint}");
    }
}

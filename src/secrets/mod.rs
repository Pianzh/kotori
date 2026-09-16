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
//!   * 凭据每个系统各存一份(Secret Service / Windows 凭据管理器),密钥环不跨系统同步;
//!   * Windows 上 `%APPDATA%` 的用户 ACL 就是明文文件的 0600,将来接上凭据管理器后更强。
//!
//! 只有 Linux 的密钥环后端(Secret Service,通过 `secret-tool`)实现了;Windows 的后端
//! 属于 Windows 移植的一部分(HANDOVER.md),在那之前明文文件就是 Windows 的落点。

/// The master-password file backend, public so callers can ask about its
/// format version and password rules.
pub mod encrypted;
/// 明文凭据文件(默认落点)。
pub mod plain;

pub use encrypted::EncryptedFile;

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
}

impl SecretKey {
    pub const ALL: [SecretKey; 2] = [Self::B2KeyId, Self::B2AppKey];

    /// Keyring "account" attribute; also what the user types into `secret-tool`.
    pub fn account(self) -> &'static str {
        match self {
            Self::B2KeyId => "b2-key-id",
            Self::B2AppKey => "b2-app-key",
        }
    }

    /// Human-readable label shown by keyring managers.
    pub fn label(self) -> &'static str {
        match self {
            Self::B2KeyId => "kotori: B2 key id",
            Self::B2AppKey => "kotori: B2 application key",
        }
    }
}

/// What to do about a missing keyring, **in the terms of the system the user is
/// actually on**.
///
/// Kept in exactly one place: the first version of this message told everyone
/// to start `kwalletd6` and add it to their niri config, which is nonsense on a
/// GNOME box, inside a container, and on Windows. Platform advice belongs next
/// to the other platform switches (see HANDOVER.md ADR-011).
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

mod keyring;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

pub use keyring::Keyring;

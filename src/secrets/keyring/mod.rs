//! 密钥环本身：一个 [`Keyring`] 背后可能是四种存储里的哪一种，以及"现在用的是
//! 哪一种"。
//!
//! 与 `entries` 分开：这里管**选择与身份**（挑后端、报状态、把明文搬进刚可用的
//! 密钥环），`entries` 管**读写一条条秘密**；`secrets/mod.rs` 只留类型与文档。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use super::encrypted::EncryptedFile;
use super::plain::PlainFile;
use super::{PROBE_ACCOUNT, SERVICE, SecretError, SecretKey, StoreKind, TOOL_ENV, backend_name};

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
/// `pub(crate)` 而不是私有：`secrets` 的单测要直接驱动这一步（"密钥环一接管，
/// 明文就搬过去"必须能被注入，不能指望这台机器恰好在跑密钥环）。
pub(crate) fn adopt_plain_entries(keyring: &Keyring, plain_path: &Path) {
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
    /// ⚠ 从前这里还返回一个 `retry` 标志(意思是"落到了内存,过一会儿可以再探一次密钥环"),
    /// 但它**恒为 `false`**:明文凭据文件这条兜底永远成立,所以"什么都存不住"在生产路径上
    /// 到不了,那段"密钥环后起来了就接管"的代码是死的(2026-09-13 核对后删掉)。真遇到
    /// 密钥环晚一步起来(比如 niri 下后来手动起了 `ksecretd`),**重启 daemon** 就会走到
    /// ② 并把明文里的凭据搬进密钥环。
    pub fn open_default(encrypted_path: &Path, plain_path: &Path) -> Self {
        Self::open_default_with(Self::system(), encrypted_path, plain_path)
    }

    /// [`open_default`] with the keyring probe passed in.
    ///
    /// Split out so the *policy* — which store wins — can be tested without the
    /// machine the test happens to run on deciding the answer. That is not
    /// hypothetical: the fallback test passed on a niri session (no Secret
    /// Service provider running) and failed on a Plasma one (ksecretd is up), and
    /// neither result said anything about the policy.
    /// `pub(crate)`：只给测试注入"本机没有密钥环"，生产路径走 [`Self::open_default`]。
    pub(crate) fn open_default_with(
        keyring: Result<Self, SecretError>,
        encrypted_path: &Path,
        plain_path: &Path,
    ) -> Self {
        let encrypted = EncryptedFile::new(encrypted_path);
        if encrypted.exists() {
            return Self::from_encrypted(encrypted);
        }

        if let Ok(keyring) = keyring {
            adopt_plain_entries(&keyring, plain_path);
            return keyring;
        }

        tracing::info!(
            "没有运行中的系统密钥环，凭据存到明文文件 {}（权限 0600）",
            plain_path.display()
        );
        Self::plain_file(plain_path)
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

    /// `pub(crate)`：测试要能确认"内存那一级从不 shell 出去"。
    pub(crate) fn run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<std::process::Output, SecretError> {
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

        // 子进程可能**没读 stdin 就已经退出了**(密钥环没在跑时 `secret-tool` 正是
        // 如此:往 stderr 报一句然后 exit 1)⇒ 这一次写会拿到 EPIPE。真正的答案在
        // 退出码和 stderr 里,所以这里只记下来、不提前返回 —— 否则同一个故障会时而
        // 报「密钥环没有在运行」、时而报「Broken pipe」,而后者对用户毫无意义。
        let mut write_error = None;
        if let Some(value) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| SecretError::Command("无法写入 secret-tool 标准输入".to_string()))?;
            if let Err(e) = pipe
                .write_all(value.as_bytes())
                .and_then(|_| pipe.write_all(b"\n"))
            {
                write_error = Some(e);
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| SecretError::Command(e.to_string()))?;

        // 写不进去、命令却"成功" = 凭据根本没存下来,不许当成功。
        if output.status.success()
            && let Some(e) = write_error
        {
            return Err(SecretError::Command(e.to_string()));
        }
        Ok(output)
    }
}

mod entries;

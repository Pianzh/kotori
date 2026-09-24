//! Where kotori keeps things, and how the config file is read and written.
//!
//! Every path is injectable through the environment (ADR-006), which is what lets
//! the integration tests run a real daemon against a temporary config.

use std::path::{Path, PathBuf};

use super::Config;

/// 守护进程的本机端点。
///
/// Linux 上是 `$XDG_RUNTIME_DIR/kotori.sock`(兜底 `/tmp`);Windows 上是命名管道
/// `\\.\pipe\kotori-<用户>` —— 那边的"socket 路径"装的是管道名,`PathBuf` 只是
/// 个一路传得下去的字符串容器,还原成名字的地方在 `daemon::ipc`。
///
/// 管道名带用户名:Windows 的 `\\.\pipe\` 是**全机器**命名空间,不带后缀的话
/// 同一台机器上两个用户的 kotori 会互相抢(而 Unix 那边靠 runtime 目录天然隔开)。
pub fn default_socket_path() -> PathBuf {
    #[cfg(unix)]
    {
        dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("kotori.sock")
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_string());
        PathBuf::from(format!(r"\\.\pipe\kotori-{user}"))
    }
}

/// Path of the config file. `KOTORI_CONFIG` overrides it (used by tests and
/// portable installs).
///
/// 只有两个地点(用户 2026-09-19 定,不做自定义):**二进制同目录**与**平台默认
/// 目录**,启动时优先搜前者 —— 便携安装把 `config.toml` 放在 `kotori.exe` 旁边,
/// 配置就跟着程序走。没有配置时"默认"就是配置地点(先有鸡才有蛋),所以新装用户
/// 的配置落在默认目录;daemon 启动时记住实际路径并成为唯一写者,"这次从哪读"与
/// "之后存到哪"因此永远一致。
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_CONFIG") {
        return PathBuf::from(p);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    choose_config_path(exe_dir.as_deref(), &default_config_dir())
}

/// Where the config lands by platform (`~/.config/kotori` / `%APPDATA%\kotori`).
fn default_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kotori")
}

/// 便携地点:二进制同目录的 `config.toml`(便携安装把配置放在 `kotori.exe` 旁边)。
///
/// 取不到自己的位置时是 `None` —— 那时候"便携"这个选项根本不存在,界面不该给按钮。
pub fn portable_config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("config.toml")))
}

/// 平台默认地点 —— 与 [`config_path`] 的兜底是同一个地方。
pub fn default_config_path() -> PathBuf {
    default_config_dir().join("config.toml")
}

/// 配置地点是不是被环境变量钉死了(`KOTORI_CONFIG`,测试与便携脚本在用)?
///
/// 钉死时"切换来源"这件事没有意义 —— 下次启动还是那个路径赢。
pub fn config_path_is_pinned() -> bool {
    std::env::var_os("KOTORI_CONFIG").is_some()
}

/// 把生效的配置从 `current` 搬到 `target`(内容就是内存里那一份)。
///
/// `disable_current`:那份**被留下**的旧文件要不要让路。切到平台默认目录时必须是
/// `true` —— "二进制同目录优先"是启动时的搜索规则,不把它挪走,下次启动它照样赢,
/// 用户看到的是"切换成功但一切照旧"。切到便携时不用管默认那份:它本来就排在后面。
///
/// 让路的做法是**改名**(`config.toml.portable-bak`)而不是删:这是用户自己的配置,
/// 搬错了还能拿回来。返回被改名的那个路径(没动就是 `None`)。
pub fn relocate_config(
    current: &Path,
    target: &Path,
    config: &Config,
    disable_current: bool,
) -> anyhow::Result<Option<PathBuf>> {
    save_to(target, config)?;
    if !disable_current || current == target || !current.is_file() {
        return Ok(None);
    }
    let backup = current.with_extension("toml.portable-bak");
    std::fs::rename(current, &backup)?;
    Ok(Some(backup))
}

/// The search rule as a pure function: a `config.toml` sitting next to the
/// binary wins, the platform default is the fallback.
fn choose_config_path(exe_dir: Option<&Path>, fallback_dir: &Path) -> PathBuf {
    if let Some(dir) = exe_dir
        && dir.join("config.toml").is_file()
    {
        return dir.join("config.toml");
    }
    fallback_dir.join("config.toml")
}

/// Path of the master-password credential file.
///
/// It lives next to the config rather than in the data directory because it is
/// configuration-shaped: one per machine, not per dataset. `KOTORI_SECRETS_FILE`
/// overrides it (tests rely on that).
pub fn secrets_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_SECRETS_FILE") {
        return PathBuf::from(p);
    }
    config_path()
        .parent()
        .map(|dir| dir.join("secrets.json"))
        .unwrap_or_else(|| PathBuf::from("secrets.json"))
}

/// Path of the **plaintext** credential file (0600) — the default store.
///
/// 和 [`secrets_path`] 放一起、名字区分开,理由一样:它是"每台机器一份"的配置形状。
/// `KOTORI_SECRETS_FILE` 一设,两者都落到同一个目录里(测试靠这个隔离,不会碰到真机的
/// `~/.config/kotori/`)。
pub fn plain_secrets_path() -> PathBuf {
    secrets_path()
        .parent()
        .map(|dir| dir.join("credentials.json"))
        .unwrap_or_else(|| PathBuf::from("credentials.json"))
}

/// Data directory (`~/.local/share/kotori`). `KOTORI_DATA_DIR` overrides it.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kotori")
}

/// Directory holding daemon logs. **跟随配置文件所在目录**(用户 2026-09-19:
/// "log跟随配置文件地址")—— 便携安装的日志也就跟着程序走;解析不出配置目录时
/// 才退回数据目录。
pub fn log_dir() -> PathBuf {
    config_path()
        .parent()
        .map(|dir| dir.join("logs"))
        .unwrap_or_else(|| data_dir().join("logs"))
}

/// Resolve the daemon socket for a given config.
/// `KOTORI_SOCKET` overrides the config value.
pub fn resolve_socket(config: &Config) -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_SOCKET") {
        return PathBuf::from(p);
    }
    config.daemon.socket_path.clone()
}

/// Resolve the daemon socket, loading the config (falling back to defaults).
pub fn socket_path() -> PathBuf {
    resolve_socket(&load().unwrap_or_default())
}

/// Load the user config.
///
/// A missing file yields defaults. A *corrupt* file is moved aside to
/// `<path>.corrupt` and defaults are returned, so a broken config never bricks
/// the app while the user's data stays recoverable (GOALS §6.2).
pub fn load() -> anyhow::Result<Config> {
    load_at(&config_path())
}

/// [`load`] against an explicit path.
///
/// The daemon remembers the file it was started with instead of re-resolving it
/// on every write, so a config that was loaded from one path can never be saved
/// over another.
pub fn load_at(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    match load_from(path) {
        Ok(config) => Ok(config),
        Err(err) => {
            let backup = path.with_extension("toml.corrupt");
            let moved = std::fs::rename(path, &backup).is_ok();
            if moved {
                tracing::error!(
                    "配置解析失败，已备份到 {}，本次使用默认配置: {err}",
                    backup.display()
                );
            } else {
                tracing::error!("配置解析失败，本次使用默认配置: {err}");
            }
            Ok(Config::default())
        }
    }
}

/// Load and parse a config from an explicit path (strict: no fallback).
pub fn load_from(path: &Path) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let mut config: Config = toml::from_str(&content)?;
    config.normalize();
    Ok(config)
}

/// Save the config to the default path.
pub fn save(config: &Config) -> anyhow::Result<()> {
    save_to(&config_path(), config)
}

/// Save the config to an explicit path, creating parent directories.
///
/// 原子写：先写同目录的临时文件、`sync_all`，再 `rename` 覆盖。直接
/// `std::fs::write` 会先把原文件截断，写到一半断电/进程被杀就只剩半份 TOML ——
/// 而读取方把"读不懂"当成"配置坏了"，于是把它搬去 `.corrupt` 并回退默认值，
/// 用户的库就此看不见了（BUG-15）。
pub fn save_to(path: &Path, config: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    let tmp = path.with_extension("toml.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut file, content.as_bytes())?;
    file.sync_all()?;
    drop(file);
    // 沿用原文件的权限（原件可能是 0600，新建的文件会退回默认值）。
    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, metadata.permissions());
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 测试用的唯一临时目录。**给整个 crate 的测试用**(`config::test_scratch`):
/// 好几处都要一个"绝不与别人重名、跑完能删掉"的目录,而重名这件事已经在
/// `Scratch` 那里咬过一次(见下面的说明),没必要每个文件各写一遍。
#[cfg(test)]
pub(crate) fn test_scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kotori-scratch-{}-{tag}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory that removes itself on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        /// `tag` 只用来让失败信息看得懂,唯一性靠那个计数器:同一进程里两个测试
        /// 用同一个 tag 是常态(好几个测试都拿"default"当兜底目录),光靠 tag +
        /// 进程 id 会让它们指向同一个路径 —— 一个的 `remove_dir_all` 插进另一个的
        /// `mkdir → is_dir` 之间,`create_dir_all` 就会返回 `AlreadyExists`
        /// (实测在 CI 上红过一次)。目录在 `Drop` 里就删了,计数器不会重复。
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "kotori-paths-{}-{tag}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_config_next_to_the_binary_wins_over_the_default() {
        let exe_dir = Scratch::new("portable");
        let fallback = Scratch::new("default");
        std::fs::write(exe_dir.0.join("config.toml"), "").unwrap();

        assert_eq!(
            choose_config_path(Some(&exe_dir.0), &fallback.0),
            exe_dir.0.join("config.toml")
        );
    }

    #[test]
    fn without_a_beside_binary_config_the_default_directory_is_used() {
        let exe_dir = Scratch::new("empty");
        let fallback = Scratch::new("default");

        assert_eq!(
            choose_config_path(Some(&exe_dir.0), &fallback.0),
            fallback.0.join("config.toml")
        );
    }

    #[test]
    fn no_exe_information_falls_back_to_the_default() {
        let fallback = Scratch::new("noexe");

        assert_eq!(
            choose_config_path(None, &fallback.0),
            fallback.0.join("config.toml")
        );
    }
}

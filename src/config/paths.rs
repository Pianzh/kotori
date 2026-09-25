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

/// 现在生效的配置是不是**便携**那一份(二进制旁边的 `config.toml`)。
///
/// 便携不只是"路径不同",它是一条行为约定:**整个目录拷走就得能用**。所以凭据
/// 不许进系统密钥环、数据不许落到 `%APPDATA%` / `~/.local/share`(用户
/// 2026-09-25 定的目标,BUG-27)。
///
/// 判断看的是**文件系统里的现状**,不是一个开关:切换配置来源会把文件搬到位
/// (见 [`relocate_config`]),搬完这里自然就跟着变。
pub fn is_portable_config() -> bool {
    config_is_portable(&config_path(), portable_config_path().as_deref())
}

/// [`is_portable_config`] 的判据本身 —— 纯函数才测得了(真实那一半要看测试二进制
/// 旁边有没有 `config.toml`)。
fn config_is_portable(effective: &Path, portable: Option<&Path>) -> bool {
    portable.is_some_and(|portable| portable == effective)
}

/// 凭据路径是不是被环境变量钉死了(`KOTORI_SECRETS_FILE`,测试靠它隔离)?
///
/// 钉死时"换配置来源要连凭据一起搬"没有意义 —— 下次启动还是那个路径赢。
pub fn secrets_path_is_pinned() -> bool {
    std::env::var_os("KOTORI_SECRETS_FILE").is_some()
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
    // 换地方也是一次"写配置",所以它也拿锁 —— 锁的是**新**地点那一把,因为
    // "以后存到哪"从此就是它(与 `Daemon::mutate_config`、`kotori add` 的直写
    // 用同一族锁,见 `config::lock`)。
    let _lock = super::ConfigLock::acquire(target)?;
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
///
/// **便携配置跟着配置目录走**:整个目录拷走就得能用,一个字节都不该落在
/// `%APPDATA%` / `~/.local/share`(用户 2026-09-25 给便携定的目标,BUG-27)。
/// 索引缓存、kopia 的本地仓库记录(`target.txt`)、daemon.lock、kwin 脚本、wine
/// prefix 表都在这个目录里 —— 所以这一条决定的是"拷走之后还连不连得上"。
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KOTORI_DATA_DIR") {
        return PathBuf::from(p);
    }
    data_dir_for(
        is_portable_config(),
        &config_path(),
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("kotori"),
    )
}

/// [`data_dir`] 的判据 —— 纯函数才测得了(真实那一半要看测试二进制旁边有没有
/// `config.toml`,而那是构建目录,不该往里写东西)。
fn data_dir_for(portable: bool, config_path: &Path, platform_default: PathBuf) -> PathBuf {
    if portable && let Some(dir) = config_path.parent() {
        return dir.join("data");
    }
    platform_default
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
///
/// ⚠ **读不动**与**读不懂**是两件事，别混成一条路：读不动（权限、I/O 错误）如实
/// 报错，既不把文件搬去 `.corrupt`、也不回退默认值 —— 否则下一次保存会把一份默认
/// 配置写到原路径上，用户的库就此不见（BUG-15 的另一半）。只有真的解析不了，才按
/// 下面那条老规矩备份并回退。
pub fn load_at(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    // 「读不动」在这里先挡住：权限不对、路径其实是个目录、盘掉了 —— 这些是"这台机器
    // 现在读不到它"，不是"这份配置坏了"。读全文一次（配置很小，多读一次不值得省），
    // 之后剩下的失败就只可能是"读不懂"。
    if let Err(err) = std::fs::read_to_string(path) {
        // 刚好被删掉：与"还没有配置"同一条路。
        if err.kind() == std::io::ErrorKind::NotFound {
            return Ok(Config::default());
        }
        return Err(anyhow::anyhow!("读不了配置文件 {}: {err}", path.display()));
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
///
/// [`load_at`] 先探一次"读不读得到"，再把它当严格解析用 —— 两条路的区别在调用方。
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
mod tests;

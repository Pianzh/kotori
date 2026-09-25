//! 配置文件上的**跨进程**锁。
//!
//! 为什么需要:配置有两个写者 —— daemon(它是唯一写者,见 `Daemon::mutate_config`)
//! 与 `kotori add` 在没有 daemon 时的直写兜底(见 `cli::add_cli`)。两边都做
//! "读-改-写",谁后写谁赢,先写的那一笔就没了(BUG-16)。[`super::save_to`] 的原子
//! 替换只保证**读到的不是半份**,不保证**两次修改都在** —— 那是这把锁的事。
//!
//! 锁是一个独立的 `<配置文件名>.lock` 邻居(不是配置本身:拿配置文件当锁会让
//! "谁来创建它"变成一个额外的状态),由操作系统在进程退出(哪怕是被杀)时自动
//! 释放 —— 不会留下"上次崩了,现在谁都进不来"的死锁。这一点是选 `flock` /
//! `LockFileEx` 而不是"自己造一个锁文件"的全部理由。

use std::path::{Path, PathBuf};

/// 持锁期间别的写者进不来;`Drop`(或进程退出)自动释放。
#[derive(Debug)]
pub struct ConfigLock {
    _file: std::fs::File,
}

impl ConfigLock {
    /// 一直等到拿到锁。等的通常只是"另一个进程写下几 KB 配置"的时间。
    pub fn acquire(config_path: &Path) -> anyhow::Result<Self> {
        let file = open_lock_file(&lock_path_for(config_path))?;
        lock(&file, true)?;
        Ok(Self { _file: file })
    }

    /// 拿不到就立刻给 `None`。
    ///
    /// **只有测试用**:检查"第二个人确实被挡住了"不能靠在测试里真去等一把锁
    /// (那会把"锁坏了"变成"测试卡住")。生产那两个调用方要的都是"等一小会儿",
    /// 也就是 [`ConfigLock::acquire`]。
    #[cfg(test)]
    pub fn try_acquire(config_path: &Path) -> anyhow::Result<Option<Self>> {
        let file = open_lock_file(&lock_path_for(config_path))?;
        match lock(&file, false) {
            Ok(()) => Ok(Some(Self { _file: file })),
            // "别人占着"不是错误:想不想等由调用方决定。
            Err(error) if would_block(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        // 关掉 fd 本身就会放锁,进程被杀也一样;显式解一次只是让意图写在纸上。
        let _ = unlock(&self._file);
    }
}

/// `<配置文件名>.lock`,与配置同目录。例如 `/etc/kotori/config.toml` →
/// `/etc/kotori/config.toml.lock`。
fn lock_path_for(config_path: &Path) -> PathBuf {
    let mut name = config_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("config.toml"));
    name.push(".lock");
    config_path.with_file_name(name)
}

fn open_lock_file(path: &Path) -> anyhow::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?)
}

// ── Unix:flock ──────────────────────────────────────────────────────────────
//
// 锁挂在**打开的文件描述**上,所以同一个进程里两次 `open` 也会互斥 —— 测试正是
// 靠这一点在单进程内验证"第二个人被挡住"。

#[cfg(unix)]
fn lock(file: &std::fs::File, wait: bool) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let mut flags = libc::LOCK_EX;
    if !wait {
        flags |= libc::LOCK_NB;
    }
    // SAFETY: fd 属于 `file`,调用期间 `file` 活着;flock 只碰内核里的锁表。
    let rc = unsafe { libc::flock(file.as_raw_fd(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: 同 `lock`。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(unix, test))]
fn would_block(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::EWOULDBLOCK)
}

// ── Windows:LockFileEx ──────────────────────────────────────────────────────

#[cfg(windows)]
fn lock(file: &std::fs::File, wait: bool) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
    if !wait {
        flags |= LOCKFILE_FAIL_IMMEDIATELY;
    }
    // SAFETY: 句柄来自 `file`(调用期间活着);`OVERLAPPED` 是本栈上的结构,
    // 同步加锁时内核只读它。
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // SAFETY: 同 `lock`。
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(windows, test))]
fn would_block(error: &std::io::Error) -> bool {
    /// `ERROR_LOCK_VIOLATION`:加锁时别人正占着。
    const ERROR_LOCK_VIOLATION: i32 = 33;
    error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁文件是配置的邻居,而且**不是**配置自己 —— 拿配置当锁会让"谁先创建它"
    /// 变成一个新的状态,而这是这把锁要避免的那类问题。
    #[test]
    fn the_lock_lives_beside_the_config() {
        assert_eq!(
            lock_path_for(Path::new("/tmp/kotori/config.toml")),
            PathBuf::from("/tmp/kotori/config.toml.lock")
        );
    }

    /// 第二个写者必须被挡在外面 —— 这就是 BUG-16 要的那件事:"读-改-写"期间
    /// 别人插不进来。
    #[test]
    fn a_second_writer_is_locked_out_until_the_first_one_finishes() {
        let dir = crate::config::test_scratch("config-lock");
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");

        let first = ConfigLock::acquire(&config).unwrap();
        assert!(
            ConfigLock::try_acquire(&config).unwrap().is_none(),
            "第一个人还拿着锁,第二个人不该拿到"
        );

        drop(first);
        let second = ConfigLock::try_acquire(&config).unwrap();
        assert!(second.is_some(), "放开之后该拿得到");
        drop(second);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

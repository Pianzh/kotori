//! `config::paths` 的测试:配置写入的原子性、便携判据与数据目录。
//!
//! 从 `config/paths.rs` 整段搬来(那边加了 BUG-27 的两条判据之后越过了 500 行软线);
//! 内部一行未改,只是换了个文件。

use super::*;

/// 便携配置的数据目录就是配置目录旁边的 `data/` —— 索引缓存、kopia 的本地
/// 仓库记录、daemon.lock 全在里面,不跟着走的话"整个目录拷走"带走的只是一半
/// (BUG-27)。
#[test]
fn a_portable_config_keeps_its_data_next_to_itself() {
    let platform = PathBuf::from("/home/u/.local/share/kotori");
    assert_eq!(
        data_dir_for(true, Path::new("/opt/kotori/config.toml"), platform.clone()),
        PathBuf::from("/opt/kotori/data")
    );
    assert_eq!(
        data_dir_for(
            false,
            Path::new("/home/u/.config/kotori/config.toml"),
            platform.clone()
        ),
        platform
    );
}

/// "便携"的判据:生效的那份配置就是**二进制旁边**那份。
#[test]
fn portable_means_the_config_sits_next_to_the_binary() {
    let beside = Path::new("/opt/kotori/config.toml");
    assert!(config_is_portable(beside, Some(beside)));
    assert!(!config_is_portable(
        Path::new("/home/u/.config/kotori/config.toml"),
        Some(beside)
    ));
    // 取不到自己的位置时,"便携"这个选项根本不存在。
    assert!(!config_is_portable(beside, None));
}

/// 读不动 ≠ 配置坏了：前者如实报错，既不把文件搬去 `.corrupt`、也不回退默认值
/// —— 否则某一次保存会把一份默认配置写到原路径上，用户的库就此不见（BUG-15
/// 的另一半）。
#[test]
fn an_unreadable_config_is_an_error_not_a_fresh_start() {
    // 把路径指到一个**目录**上：读它必然失败，而且不是 NotFound（那个仍然是
    // "还没有配置"）。
    let dir = test_scratch("config-unreadable");
    let error = load_at(&dir).expect_err("读不动时必须报错");
    assert!(error.to_string().contains("读不了配置文件"), "{error}");
    assert!(dir.exists(), "读不动不许把它搬走");
}

/// 原子写的证明：一边写、一边读，读到的永远是**完整的**一份（GAP-2 的一半）。
///
/// 只跑 unix：Windows 上 `rename` 撞上"正被打开的文件"会失败，那是另一套文件
/// 共享语义（生产路径里 daemon 是唯一写者，读方是同一个进程用 RwLock 串着的，
/// 撞不上）。
#[cfg(unix)]
#[test]
fn a_reader_never_sees_a_half_written_config() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = test_scratch("config-atomic");
    let path = dir.join("config.toml");
    // 写一份**很长**的值：写到一半被读到，长度就会短一截（或者 TOML 直接读不懂）。
    let long = "x".repeat(200_000);

    // 先落一份，免得把"还没有文件"（那是默认值）误判成"读到半份"。
    let mut initial = Config::default();
    initial.daemon.socket_path = PathBuf::from(&long);
    save_to(&path, &initial).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let path = path.clone();
        let stop = stop.clone();
        let long = long.clone();
        std::thread::spawn(move || {
            for _ in 0..50 {
                let mut config = Config::default();
                config.daemon.socket_path = PathBuf::from(&long);
                save_to(&path, &config).unwrap();
            }
            stop.store(true, Ordering::Relaxed);
        })
    };

    let mut reads = 0;
    while !stop.load(Ordering::Relaxed) {
        let loaded = load_at(&path).expect("读到半份配置：原子写没生效");
        assert_eq!(
            loaded.daemon.socket_path.to_string_lossy().len(),
            long.len(),
            "读到的必须是完整的一份"
        );
        reads += 1;
    }
    writer.join().unwrap();
    assert!(reads > 0, "读线程一次都没跑");
    std::fs::remove_dir_all(&dir).ok();
}

/// 崩在写入中途留下的临时文件不该影响读取（GAP-2 的另一半）：读取只看正式那份，
/// `.tmp` 是写到一半的残骸 —— 这也正是"临时文件 + rename"这个写法的另一半好处。
#[test]
fn a_leftover_temp_file_does_not_disturb_the_config() {
    let dir = test_scratch("config-leftover");
    let path = dir.join("config.toml");
    let mut config = Config::default();
    config.daemon.socket_path = PathBuf::from("/run/kotori.sock");
    save_to(&path, &config).unwrap();

    // 模拟"写到一半被杀"：同目录里留下一份半截的临时文件。
    std::fs::write(path.with_extension("toml.tmp"), "[daemon").unwrap();

    let loaded = load_at(&path).expect("临时文件不该影响读取");
    assert_eq!(loaded.daemon.socket_path, PathBuf::from("/run/kotori.sock"));
    std::fs::remove_dir_all(&dir).ok();
}

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

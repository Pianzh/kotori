//! 云端索引的**本地缓存**：拿下来一次就存着，之后平时都在本地查。
//!
//! 为什么要有它（用户 2026-09-23 点出来的）：读一次云端索引仍然是一次网络往返 ——
//! rclone 要 `cat` 两个对象，kopia 要列快照再 `restore` 一条。而"云端有哪些游戏"这件事
//! 打开页面要问、填完 exe 要问、点「自己选…」要问。索引本身是**不常变**的东西（用户原话），
//! 所以：读到一次就落到本机，之后默认一律看本地；只有三种时候真的去云端 ——
//! **用户按「刷新」**、**第一次**（本地还没有）、**上传成功/深扫之后**（那时我们自己刚写过
//! 云端那份，顺手把本地这份也更新掉，连读都省了）。
//!
//! ⚠ **按云目标签名分开存**（引擎 + endpoint + bucket + prefix，见 `crate::sync::signature`）：
//! 换桶之后读到旧桶的清单是最坏的一种错 —— 那会把 A 桶的身份绑到 B 桶的档案上。所以文件里
//! 记着签名，对不上就当没有。
//!
//! ⚠ 读不懂、格式不认识、签名对不上 —— 一律当**没有缓存**（绝不猜）。代价只是白跑一趟网络，
//! 而猜错的代价是一条错的绑定。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::index::{CloudIndex, stamp};

/// 缓存放在数据目录下的这个子目录里（一个云目标一个文件）。
pub const CACHE_DIR: &str = "cloud-index";
/// 缓存的格式版本。**不认识就当读不懂**。
pub const CACHE_FORMAT: u32 = 1;

/// 缓存**多旧算旧**（用户 2026-09-23 定的：一个钟头）。
///
/// 它同时是那个后台循环的周期（见 `crate::daemon::index_refresh`），以及读路径"要不要顺手
/// 刷一次"的判据 —— 所以只有这一个数，别在两处各写一份。
pub const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// 本机存着的那一份云端索引。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedIndex {
    pub format: u32,
    /// 这一份是**哪个云目标**的（见 `crate::sync::signature`）。
    pub signature: String,
    /// 什么时候拿下来的（[`stamp`] 形状 —— 界面直接拿 `describe_stamp` 转人话）。
    pub cached_at: String,
    /// 云端那份索引。`None` = 拿的时候桶里**还没有**索引 —— 那与"没有缓存"是两件事
    /// （前者可以安心说"去深扫一次"，后者要先去看一眼才知道）。
    pub index: Option<CloudIndex>,
}

impl CachedIndex {
    /// 刚拿到的那一份。
    pub fn new(signature: &str, index: Option<CloudIndex>) -> Self {
        Self {
            format: CACHE_FORMAT,
            signature: signature.to_string(),
            cached_at: stamp(),
            index,
        }
    }
}

/// 缓存文件落在哪儿。签名里有 `:`、`/` 这些不能当文件名的字符，所以文件名取它的哈希
/// （签名本身存在文件里，读的时候校验）。
pub fn path_in(root: &Path, signature: &str) -> PathBuf {
    use blake2::{Blake2b256, Digest};

    let mut hasher = Blake2b256::new();
    hasher.update(signature.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    root.join(CACHE_DIR).join(format!("{hex}.json"))
}

/// 读缓存。文件不在、读不懂、格式不认识、签名对不上 —— 一律 `None`（当没有）。
pub fn read_at(root: &Path, signature: &str) -> Option<CachedIndex> {
    let path = path_in(root, signature);
    let text = std::fs::read_to_string(&path).ok()?;
    let cached: CachedIndex = match serde_json::from_str(&text) {
        Ok(cached) => cached,
        Err(error) => {
            tracing::warn!(
                "云端索引缓存读不懂（当作没有缓存）: {}: {error}",
                path.display()
            );
            return None;
        }
    };
    if cached.format != CACHE_FORMAT {
        tracing::warn!(
            "云端索引缓存的格式不认识（当作没有缓存）: {}（格式 {}）",
            path.display(),
            cached.format
        );
        return None;
    }
    if cached.signature != signature {
        tracing::warn!(
            "云端索引缓存的签名对不上（当作没有缓存）: {}",
            path.display()
        );
        return None;
    }
    // 缓存里**嵌着**的那份索引也要过格式这一关：外层是我们的缓存格式，里层是云端
    // 写下的索引格式，未来版本写的东西不该被这一版按当前字段解释（BUG-25）。
    if let Some(index) = &cached.index
        && !index.is_supported()
    {
        tracing::warn!(
            "缓存里那份云端索引的格式不认识（当作没有缓存）: {}（格式 {}）",
            path.display(),
            index.format
        );
        return None;
    }
    Some(cached)
}

/// 写缓存。**先写临时文件再改名** —— 崩在半路也不会留下一份半截的 JSON
/// （读到它只会当没有缓存，但那意味着白跑一趟网络）。
pub fn write_at(root: &Path, cached: &CachedIndex) -> std::io::Result<()> {
    let path = path_in(root, &cached.signature);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec(cached).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-cleaning scratch directory under the system temp dir.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kotori-index-cache-{}-{}-{}",
                tag,
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn index_with(cloud_id: &str) -> CloudIndex {
        let mut identity = crate::sync::cloud::GameIdentity::new(cloud_id, "示例");
        identity.merge_machine(crate::sync::cloud::MachineIdentity {
            machine_id: "m".to_string(),
            label: "host".to_string(),
            fingerprints: vec!["v1:1:aa".to_string()],
            locations: Vec::new(),
            parents: Vec::new(),
            exe_paths: Vec::new(),
        });
        let mut index = CloudIndex::new();
        index.merge(crate::sync::index::IndexGame::from_identity(
            "key", identity,
        ));
        index
    }

    #[test]
    fn a_cached_index_round_trips_with_its_signature() {
        let dir = TempDir::new("roundtrip");
        let signature = "v1:rclone:https://host:bucket:kotori";
        assert!(read_at(&dir.0, signature).is_none(), "还没写过就是没有");

        let cached = CachedIndex::new(signature, Some(index_with("c1")));
        write_at(&dir.0, &cached).unwrap();

        let read = read_at(&dir.0, signature).expect("刚写的该读得回来");
        assert_eq!(read, cached);
        assert_eq!(read.index.as_ref().unwrap().len(), 1);
        // 临时文件不许留下（它比缓存本身更容易被误读成"坏了"）。
        assert!(
            !path_in(&dir.0, signature)
                .with_extension("json.tmp")
                .exists()
        );
    }

    /// "桶里还没有索引"也要记得住 —— 否则每次打开页面都要为"有没有"跑一趟网络。
    #[test]
    fn an_empty_cloud_is_remembered_too() {
        let dir = TempDir::new("empty");
        let signature = "v1:rclone:https://host:bucket:";
        write_at(&dir.0, &CachedIndex::new(signature, None)).unwrap();

        let read = read_at(&dir.0, signature).expect("缓存该在");
        assert!(read.index.is_none(), "记得的是「云端还没有索引」");
    }

    /// 换桶之后绝不许读到旧桶的清单 —— 那会把 A 桶的身份绑到 B 桶的档案上。
    #[test]
    fn another_target_never_reads_this_target_s_cache() {
        let dir = TempDir::new("other-bucket");
        let first = "v1:rclone:https://host:bucket-a:kotori";
        let second = "v1:rclone:https://host:bucket-b:kotori";
        write_at(&dir.0, &CachedIndex::new(first, Some(index_with("c1")))).unwrap();

        assert!(read_at(&dir.0, second).is_none(), "另一个桶就是没有缓存");
        assert!(read_at(&dir.0, first).is_some(), "自己那个还在");
    }

    /// 读不懂 / 格式不认识：当没有缓存（绝不猜），下次联网读到就覆盖。
    #[test]
    fn a_broken_or_unknown_cache_is_treated_as_missing() {
        let dir = TempDir::new("broken");
        let signature = "v1:rclone:https://host:bucket:kotori";
        let path = path_in(&dir.0, signature);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(read_at(&dir.0, signature).is_none());

        let mut cached = CachedIndex::new(signature, Some(index_with("c1")));
        cached.format = CACHE_FORMAT + 1;
        std::fs::write(&path, serde_json::to_vec(&cached).unwrap()).unwrap();
        assert!(read_at(&dir.0, signature).is_none(), "不认识的格式不许瞎读");
    }
}

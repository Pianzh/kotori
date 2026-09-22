//! exe 指纹：跨机器认"这一款就是这一款"。
//!
//! 判据是**文件大小 + 首尾各 1 MiB 的内容哈希**：一个几十 GB 的 galgame 不该因为
//! 同步拦一次就被整个读一遍，而头尾两块足够区分"同一款游戏"与"另一款游戏"（安装器
//! 的 exe 往往只有几 MB，那时读的就是整份）。
//!
//! ## 为什么是 BLAKE2b-256 而不是 sha256
//!
//! 形状与当初定的方案一致（大小 + 首尾哈希），哈希函数换成了 **BLAKE2b-256**：它已经
//! 在依赖树里（argon2 用它），为一个指纹再把 sha2 牵进来要连带 6 个 crate。指纹完全在
//! 本机算、只用来**提议**（改身份要用户点头），不参与任何密码学协议，两者在这里等价。
//!
//! ## 形状与兼容
//!
//! 字符串以 `v1:` 开头：**形状（算法、窗口）变了就换版本号**，老指纹从此对不上任何
//! 人（等于"还没有指纹"），而不是与新的碰巧相等。
//!
//! ## 已知的取舍
//!
//! 超过 2 MiB 的文件，**中间改动不会被发现**——这是"不做整文件哈希"的另一面，也是
//! 刻意的：指纹只当提议，命中与否只决定"要不要自动绑定"，不决定"要不要覆盖存档"。
//! 大小进了字符串，所以换版本、打补丁（字节数几乎必然变）都认得出。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use blake2::{Blake2b256, Digest};

/// 头、尾各读这么多字节。
const WINDOW: u64 = 1024 * 1024;
/// 指纹版本。形状变了就换它。
const VERSION: &str = "v1";

/// 算一个文件的指纹；读不到就 `None`（磁盘拔了、没权限、不是文件）。
///
/// 绝不编一个"大概"的值：`None` 的含义是"还不知道"，而一个假的指纹会让两台机器
/// 认错人 —— 那是这个功能唯一不可逆的错误。
pub fn of_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut hasher = Blake2b256::new();

    if size <= 2 * WINDOW {
        // 小文件（绝大多数 exe）：整份读一遍。**不能切头尾** —— 那样中间那段会被
        // 数两遍，于是同一个文件在不同实现下算出不同的值。
        let mut whole = Vec::with_capacity(size as usize);
        file.read_to_end(&mut whole).ok()?;
        hasher.update(&whole);
    } else {
        let mut head = vec![0u8; WINDOW as usize];
        file.read_exact(&mut head).ok()?;
        hasher.update(&head);
        file.seek(SeekFrom::End(-(WINDOW as i64))).ok()?;
        let mut tail = vec![0u8; WINDOW as usize];
        file.read_exact(&mut tail).ok()?;
        hasher.update(&tail);
    }

    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(format!("{VERSION}:{size}:{hex}"))
}

/// 给还没有指纹的档案补上（本机的事，与云端无关）。
///
/// **只补缺失的**：已经有指纹的一条都不碰。exe 换了之后要不要跟着换，是用户的事
/// （指纹只当提议）——在背后悄悄换掉它，等于把"这一款还是不是原来那一款"替用户
/// 改了答案。
///
/// 读不到 exe（盘不在、没权限）就跳过，下次再来。幂等，所以不需要任何"补过了"的
/// 标记。返回这次真的补上的那些 id（有序，方便界面上说"已为 N 款游戏建立指纹"）。
pub fn fill_missing(config: &mut crate::config::Config) -> Vec<String> {
    let mut filled = Vec::new();
    for (id, game) in config.games.iter_mut() {
        if game.exe_fingerprint.is_some() {
            continue;
        }
        if let Some(print) = of_file(&game.exe_path) {
            game.exe_fingerprint = Some(print);
            filled.push(id.clone());
        }
    }
    filled.sort();
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kotori-fingerprint-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_fingerprint_says_what_it_is_and_how_big() {
        let dir = temp("shape");
        let exe = dir.join("game.exe");
        std::fs::write(&exe, b"hello").unwrap();

        let print = of_file(&exe).unwrap();
        let parts: Vec<&str> = print.split(':').collect();
        assert_eq!(parts.len(), 3, "{print}");
        assert_eq!(parts[0], "v1");
        assert_eq!(parts[1], "5", "大小在里面：换版本、打补丁都会变");
        assert_eq!(parts[2].len(), 64, "BLAKE2b-256 是 32 字节: {print}");
        assert!(parts[2].chars().all(|c| c.is_ascii_hexdigit()), "{print}");

        // 同一个文件算两次必须一样（否则"粘住"的提议毫无意义）。
        assert_eq!(of_file(&exe).unwrap(), print);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_two_megabyte_boundary_does_not_double_count_bytes() {
        let dir = temp("boundary");
        // 恰好 2 MiB：走"整份"那条路；2 MiB + 1：走头尾那条路。两条路算出来的值
        // 必须只由内容决定，所以同样的内容在两台机器上（大小一样）一定一致。
        let exact = dir.join("exact.bin");
        let over = dir.join("over.bin");
        let body: Vec<u8> = (0..2 * WINDOW as usize).map(|i| (i % 251) as u8).collect();
        std::fs::write(&exact, &body).unwrap();
        std::fs::write(&over, [body.as_slice(), b"x"].concat()).unwrap();

        assert!(of_file(&exact).unwrap().starts_with("v1:2097152:"));
        assert!(of_file(&over).unwrap().starts_with("v1:2097153:"));

        // 大文件只读头尾：中间那段改了，指纹**不变**（已知取舍，写在模块说明里）。
        let big = dir.join("big.bin");
        let mut bytes = vec![7u8; 3 * WINDOW as usize];
        std::fs::write(&big, &bytes).unwrap();
        let before = of_file(&big).unwrap();
        bytes[WINDOW as usize + 5] = 9;
        std::fs::write(&big, &bytes).unwrap();
        assert_eq!(
            of_file(&big).unwrap(),
            before,
            "中间那段不在窗口里 —— 这是刻意的"
        );

        // 头部与尾部各改一个字节都必须认得出。
        for at in [10usize, 3 * WINDOW as usize - 10] {
            let mut changed = bytes.clone();
            changed[at] = changed[at].wrapping_add(1);
            std::fs::write(&big, &changed).unwrap();
            assert_ne!(of_file(&big).unwrap(), before, "第 {at} 字节改了");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_cannot_be_read_has_no_fingerprint() {
        let dir = temp("missing");
        assert_eq!(of_file(&dir.join("nope.exe")), None);
        // 目录不是文件：读它会失败，于是如实说"还不知道"。
        assert_eq!(of_file(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 补缺**只补缺的**：已经有指纹的一条都不碰，读不到的如实留空。
    #[test]
    fn filling_missing_fingerprints_touches_only_the_ones_without_one() {
        let dir = temp("fill");
        let exe = dir.join("game.exe");
        std::fs::write(&exe, b"content").unwrap();
        let known = of_file(&exe).unwrap();

        let mut config = crate::config::Config::default();
        config
            .games
            .insert("has-one".into(), game("has-one", &exe, Some("v1:1:keepme")));
        config
            .games
            .insert("missing".into(), game("missing", &exe, None));
        // 盘不在：这一款这次补不上。
        config.games.insert(
            "no-disk".into(),
            game("no-disk", &dir.join("gone.exe"), None),
        );

        assert_eq!(fill_missing(&mut config), vec!["missing".to_string()]);
        assert_eq!(
            config.games["missing"].exe_fingerprint.as_deref(),
            Some(known.as_str())
        );
        assert_eq!(
            config.games["has-one"].exe_fingerprint.as_deref(),
            Some("v1:1:keepme"),
            "已有的指纹不许被悄悄换掉"
        );
        assert_eq!(config.games["no-disk"].exe_fingerprint, None);

        // 幂等：再跑一次没东西可补。
        assert!(fill_missing(&mut config).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn game(name: &str, exe: &Path, fingerprint: Option<&str>) -> crate::config::GameConfig {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "exe_path": exe.to_string_lossy(),
            "exe_fingerprint": fingerprint,
            "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }
}

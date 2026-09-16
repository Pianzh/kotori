//! 一版一包：把某个游戏的全部存档位置打成一个 zip，以及从包里取回来。
//!
//! 云端布局是 `<游戏id>/<stamp>.zip`，**一个包就是一个时间点的完整存档**——
//! 不是差量。这一条是所有"回退一定回得去"的底气：恢复不需要在好几个目录之间
//! 拼凑，铺一个包就是那一刻。
//!
//! 四条规则：
//!   * 每个存档位置在包内占一个顶层目录：`<key>/<相对路径>`；
//!   * 包根一份 [`MANIFEST`]，记下每个文件的 `size` 与 `mtime_ms`，还有这一版
//!     包含哪些存档位置（位置存在但一个文件都没有，也要能被认出来）；
//!   * 判"谁新"**只看清单里的这两个数**：zip 条目自带的时间戳只有 2 秒精度，
//!     而且解包方不一定照它落盘，所以它不参与任何判断（见 `unpack`）；
//!   * 完整性交给 zip 自带的 CRC32，不额外做内容哈希——哈希是"以后想要内容级
//!     校验再加"的东西，现在加它等于多一个依赖、多一遍全量读盘。
//!
//! 这里全是纯本地函数：不碰 rclone、不碰网络、不用密钥，所以可以整段单测。
//! 文件分工：`pack` 只管"怎么装进去"，`unpack` 只管"怎么拿出来、该不该覆盖"，
//! 类型与常量留在本文件。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod gather;
mod materialize;
mod pack;
#[cfg(test)]
mod tests;
mod unpack;

pub use materialize::materialize;
pub use pack::{PackReport, pack};
pub use unpack::{MergePlan, extract, plan, read_dir_manifest};

/// 包根那份清单的文件名。
pub const MANIFEST: &str = "kotori-manifest.json";
/// 清单格式版本。将来改结构时靠它认新旧，而不是猜。
pub const FORMAT: u32 = 1;

/// How a transfer treats a file that already exists at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Merge {
    /// 以包为准，整份铺回去。这是**手动恢复**：用户点了"恢复"，就是他说了算
    /// （ADR-012 的原意）。
    Replace,
    /// 只覆盖**比本机新**的文件，本机更新过的、以及包上没有的文件一律不动。
    /// 这是启动前的自动取回：上一次上传失败（没网、机器崩了）时，本机的存档
    /// 才是最新的，自动流程绝不许吃掉用户刚打出来的进度。
    Newer,
}

/// 一个包的清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// 打包时刻（UTC、秒精度）。只给人看，不参与任何判断。
    pub created: String,
    /// 这一版包含哪些存档位置。**空目录也算包含**，所以它不能从 `entries` 推。
    /// `#[serde(default)]` 是为了还能读没有这个字段的包。
    #[serde(default)]
    pub locations: Vec<String>,
    pub entries: Vec<Entry>,
}

/// 清单里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// 存档位置的 key（`save_key` 的产物）。
    pub key: String,
    /// 相对该存档位置的路径，总是用 `/` 分隔。
    pub path: String,
    pub size: u64,
    /// 修改时间，Unix 毫秒。
    pub mtime_ms: i64,
}

impl Manifest {
    /// 这一版包含的存档位置。老包没有 `locations` 字段时从条目里推。
    pub fn locations(&self) -> BTreeSet<&str> {
        if !self.locations.is_empty() {
            return self.locations.iter().map(String::as_str).collect();
        }
        self.entries.iter().map(|e| e.key.as_str()).collect()
    }

    /// 这个存档位置在这一版里吗（哪怕它一个文件都没有）。
    pub fn has_location(&self, key: &str) -> bool {
        self.locations().contains(key)
    }
}

impl Entry {
    /// 条目在包内的完整路径（也是解包后相对临时目录的路径）。
    pub fn name(&self) -> String {
        format!("{}/{}", self.key, self.path)
    }
}

/// 文件在磁盘上的修改时间，Unix 毫秒；拿不到就记 0。
///
/// 判"谁新"靠它，所以它必须比 zip 的时间戳可靠：zip 存的是 DOS 时间（2 秒
/// 精度、还要受时区影响），而这里直接读文件系统给的答案。
pub(super) fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 把毫秒时间戳盖回一个刚解包出来的文件。
pub(super) fn set_mtime(file: &std::fs::File, ms: i64) -> Result<(), String> {
    if ms <= 0 {
        return Ok(());
    }
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64);
    file.set_modified(time)
        .map_err(|e| format!("设置修改时间失败: {e}"))
}

/// 把清单里的修改时间盖到一个文件上。
///
/// 解包后要盖（zip 自带的时间戳只有 2 秒精度），从临时目录复制到存档目录之后
/// 还要再盖一次（`fs::copy` 不保证带时间）——"谁新"全靠这个数。
pub fn apply_mtime(path: &Path, ms: i64) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("无法打开 {} 改时间: {e}", path.display()))?;
    set_mtime(&file, ms)
}

/// 包内路径必须是干净的相对路径。
///
/// 包是我们自己打的，但恢复一个别人给的、或者被改过的包时，`../../.bashrc`
/// 这种名字必须在这里就被拦住——`local.join(entry.path)` 一旦逃出存档目录，
/// 覆盖的就不只是存档了。
pub(super) fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// 把包内的 `/` 分隔路径转成本机路径。
pub(super) fn to_local_path(path: &str) -> PathBuf {
    path.split('/').collect()
}

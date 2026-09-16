//! 摆成目录：把这一版的存档原样复制到一个目录树里，外加一份清单。
//!
//! 这是给 kopia 用的"打包"。kopia 快照的是**目录树**，而一个游戏的存档位置散在
//! 好几个地方，所以先照 zip 那条路的规矩摆成 `<key>/<相对路径>`，它才能一次拍下
//! 整个游戏。摆出来的东西和 zip 里的内容一一对应，清单也是同一份 [`Manifest`]，
//! 于是恢复那条路（`unpack`）两个引擎都能走。
//!
//! 代价是本地多一次拷贝（zip 那条路是直接读原文件压缩，这里要落一份副本）。换来
//! 的是 kopia 能对**未压缩的原始文件**做内容去重——喂给它 zip 的话，压缩后的字节
//! 几乎没有重复可找，去重就白搭了。

use std::path::Path;

use super::gather::gather;
use super::pack::PackReport;
use super::{FORMAT, MANIFEST, Manifest};
use crate::sync::SaveTarget;

/// 把 `targets` 里存在的每个存档位置摆进 `dir`，并在 `dir` 根写一份清单。
///
/// `dir` 里的旧内容不会被清理：调用方给的总是一个刚建出来的空目录（`Staging`
/// 的临时目录用完即删），多一条"先清空"的路径只会多一个删错东西的机会。
pub fn materialize(
    dir: &Path,
    targets: &[SaveTarget],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PackReport, String> {
    let gathered = gather(targets)?;
    let manifest = Manifest {
        format: FORMAT,
        created: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        locations: gathered.locations,
        entries: gathered.entries,
    };

    for (key, absolute, relative) in &gathered.files {
        let destination = dir
            .join(key)
            .join(relative.split('/').collect::<std::path::PathBuf>());
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("无法创建 {}: {e}", parent.display()))?;
        }
        std::fs::copy(absolute, &destination)
            .map_err(|e| format!("无法写入 {}: {e}", destination.display()))?;
    }

    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("清单序列化失败: {e}"))?;
    std::fs::write(dir.join(MANIFEST), text)
        .map_err(|e| format!("写入清单 {} 失败: {e}", dir.join(MANIFEST).display()))?;

    Ok(PackReport {
        entries: manifest.entries,
        locations: manifest.locations,
        missing: gathered.missing,
        excluded: gathered.excluded,
    })
}

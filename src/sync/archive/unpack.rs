//! 解包与合并判定：把包读出来，并回答"这些文件里,哪些该铺到本机"。
//!
//! 这是整个同步里最需要小心的一半：ADR-012 的"自动取回只取新的"从前是
//! `rclone --update` 保证的，改成一版一包之后，这条不变量落在 [`plan`] 上。
//! 所以这里的判定必须是纯函数（给定清单和本机现状，算出该做什么），
//! 由 `runner` 去执行——一旦判定错了，是"用户刚打出来的进度被旧包盖掉"。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::{
    Entry, FORMAT, MANIFEST, Manifest, Merge, mtime_ms, safe_rel, set_mtime, to_local_path,
};
use crate::sync::SaveTarget;

/// 一个包打算怎么落到本机。
#[derive(Debug, Clone)]
pub struct MergePlan {
    /// 要铺过去的文件（覆盖式）。
    pub take: Vec<Entry>,
    /// 本机更新、因此保持不动的文件。
    pub kept: Vec<Entry>,
    /// 本机有、这个包里没有的文件（`<key>/<相对路径>`）。**默认一个都不删**：
    /// 删本地数据永远是用户点头才做的事，这里只负责如实列出来。
    pub extras: Vec<String>,
}

/// 读出一个包的清单，不解包。
pub fn read_manifest(zip_path: &Path) -> Result<Manifest, String> {
    let file =
        File::open(zip_path).map_err(|e| format!("无法打开存档包 {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("{} 不是有效的 zip 包: {e}", zip_path.display()))?;
    let mut raw = String::new();
    archive
        .by_name(MANIFEST)
        .map_err(|_| {
            format!(
                "{} 里没有 {MANIFEST}，不是 kotori 打的包",
                zip_path.display()
            )
        })?
        .read_to_string(&mut raw)
        .map_err(|e| format!("读取清单失败: {e}"))?;
    parse_manifest(&raw)
}

/// 解析清单文本，顺带拒绝"来自未来"的格式版本。
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let manifest: Manifest =
        serde_json::from_str(text).map_err(|e| format!("清单不是有效的 JSON: {e}"))?;
    if manifest.format > FORMAT {
        return Err(format!(
            "这个包是更新版本的 kotori 打的（格式 {}，本机只认到 {FORMAT}）",
            manifest.format
        ));
    }
    Ok(manifest)
}

/// 读出一份**目录形态**的清单（kopia 那条路：`materialize` 摆出来的目录）。
///
/// 与 [`read_manifest`] 是同一件事的两个入口：zip 把清单塞在包根，目录形态就
/// 把 `kotori-manifest.json` 摆在目录根，内容一模一样。
pub fn read_dir_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| format!("{} 里没有 {MANIFEST}，不是 kotori 打的版本", dir.display()))?;
    parse_manifest(&raw)
}

/// 把包解到 `into`（一个临时目录），按清单把修改时间盖回每个文件。
///
/// 时间必须自己盖：zip 条目里存的是 2 秒精度的 DOS 时间，直接用它会让"谁新"
/// 在往返一次之后变得不可判——上传→取回→再比较，本该判成"一样"，却可能因为
/// 时间被抹平而判成"本机更新"，那之后就再也取不回云端的新存档了。
pub fn extract(zip_path: &Path, into: &Path) -> Result<Manifest, String> {
    let manifest = read_manifest(zip_path)?;
    let file =
        File::open(zip_path).map_err(|e| format!("无法打开存档包 {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("{} 不是有效的 zip 包: {e}", zip_path.display()))?;

    // 远端包是**外部输入**：一个异常包不该把临时目录撑爆、也不该把 CPU 卡死
    // （BUG-28）。下面几个都是"正常存档碰不到"的量级。
    const MAX_ENTRIES: usize = 20_000;
    const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

    if archive.len() > MAX_ENTRIES {
        return Err(format!(
            "包内条目太多（{} 项，上限 {MAX_ENTRIES}），拒绝解包",
            archive.len()
        ));
    }
    let mut written_total: u64 = 0;
    for index in 0..archive.len() {
        let mut item = archive
            .by_index(index)
            .map_err(|e| format!("读取包内第 {index} 项失败: {e}"))?;
        if item.is_dir() {
            continue;
        }
        let name = item.name().to_string();
        // 包内路径由我们自己生成，所以只接受自己那套写法：`enclosed_name` 挡
        // `..` 与绝对路径，反斜杠在这里挡（Windows 风格的包体在 Linux 上会被
        // 当成一个合法文件名，静默写出一堆怪文件）。
        let Some(relative) = item.enclosed_name() else {
            return Err(format!("包内路径不安全，拒绝解包: {name}"));
        };
        if name.contains('\\') {
            return Err(format!("包内路径不安全，拒绝解包: {name}"));
        }
        let destination = into.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("无法创建 {}: {e}", parent.display()))?;
        }
        let mut out = File::create(&destination)
            .map_err(|e| format!("无法写入 {}: {e}", destination.display()))?;
        // 按**实际写出的字节**记账，不看条目自己声明的尺寸：包是远端来的，
        // 声明值可以是假的。
        let mut limited = std::io::Read::take(&mut item, MAX_FILE_BYTES + 1);
        let written = std::io::copy(&mut limited, &mut out)
            .map_err(|e| format!("解包 {} 失败: {e}", destination.display()))?;
        if written > MAX_FILE_BYTES {
            return Err(format!("包内 {name} 超过单个文件的上限，拒绝解包"));
        }
        written_total = written_total.saturating_add(written);
        if written_total > MAX_TOTAL_BYTES {
            return Err(format!(
                "包解出来的总量超过上限（{} GiB），拒绝解包",
                MAX_TOTAL_BYTES / (1024 * 1024 * 1024)
            ));
        }

        if let Some(entry) = manifest.entries.iter().find(|entry| entry.name() == name) {
            set_mtime(&out, entry.mtime_ms)?;
        }
    }

    Ok(manifest)
}

/// 算出这个包该怎么落到本机。
///
/// [`Merge::Newer`]（启动前自动取回）只覆盖**比本机新**的；[`Merge::Replace`]
/// （用户点"恢复"）整份铺回去。两边都只列不改：本机多出来的文件进 `extras`，
/// 由调用方报给用户，不在这里删。
pub fn plan(
    manifest: &Manifest,
    targets: &[SaveTarget],
    merge: Merge,
) -> Result<MergePlan, String> {
    let mut take = Vec::new();
    let mut kept = Vec::new();

    for entry in &manifest.entries {
        if !safe_rel(&entry.path) || entry.key.is_empty() {
            return Err(format!("包内路径不安全，拒绝使用: {}", entry.name()));
        }
        // 包里有一个这台机器没配置的位置：跳过它。不报错——那是另一台机器
        // （或者另一个系统）才有的目录，很正常。
        let Some(target) = targets.iter().find(|target| target.key == entry.key) else {
            continue;
        };
        let local = target.local.join(to_local_path(&entry.path));
        if should_take(entry, &local, merge) {
            take.push(entry.clone());
        } else {
            kept.push(entry.clone());
        }
    }

    Ok(MergePlan {
        take,
        kept,
        extras: extras(manifest, targets)?,
    })
}

/// 本机有、这个包里没有的文件（`<key>/<相对路径>`，已排序）。
///
/// 只列不删：恢复之后它们还在原地，游戏可能照旧读到——所以必须让用户看见。
fn extras(manifest: &Manifest, targets: &[SaveTarget]) -> Result<Vec<String>, String> {
    let known: std::collections::BTreeSet<String> =
        manifest.entries.iter().map(|entry| entry.name()).collect();
    let mut extras = Vec::new();
    for target in targets {
        if !target.local.is_dir() {
            continue;
        }
        let (found, _) = super::gather::collect_files(&target.local, &target.exclude)?;
        for (_, relative) in found {
            let name = format!("{}/{relative}", target.key);
            if !known.contains(&name) {
                extras.push(name);
            }
        }
    }
    extras.sort();
    Ok(extras)
}

/// 这个文件该不该用包里的版本盖掉本机的。
fn should_take(entry: &Entry, local: &Path, merge: Merge) -> bool {
    let Ok(meta) = std::fs::metadata(local) else {
        // 本机没有：两种模式都取。
        return true;
    };
    if !meta.is_file() {
        // 本机这个位置上是个目录：包里的文件必须落下，否则位置被占着。
        return true;
    }
    match merge {
        Merge::Replace => true,
        Merge::Newer => {
            let local_ms = mtime_ms(&meta);
            if local_ms == entry.mtime_ms {
                // 同一秒内的两次写入分不出先后，就用大小判：大小也一样才算一样。
                meta.len() != entry.size
            } else {
                local_ms < entry.mtime_ms
            }
        }
    }
}

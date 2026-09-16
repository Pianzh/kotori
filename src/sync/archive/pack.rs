//! 打包：把一个游戏在本机的存档收进一个 zip，并在包里放一份清单。
//!
//! 只做"装进去"这一半：收集文件、套用排除规则、写 zip。要不要覆盖、谁更新，
//! 全在 `unpack` 那边——那边才是会动本机数据的地方。

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::{Entry, FORMAT, MANIFEST, Manifest, mtime_ms};
use crate::sync::SaveTarget;

/// 打包过程中攒下来的东西：一个包的内容，以及它没能包含谁。
#[derive(Debug, Clone)]
pub struct PackReport {
    /// 进了包的文件，按存档位置和相对路径排好序。
    pub entries: Vec<Entry>,
    /// 这一版包含哪些存档位置（目录在，哪怕空着）。
    pub locations: Vec<String>,
    /// 本机根本没有的存档位置。报告成 `skipped`，不是错误：一台机器上没装
    /// 某个位置（只在 Windows 上才有的那个目录）是正常的。
    pub missing: Vec<String>,
    /// 打包时被排除规则挡下的文件数，只用于日志。
    pub excluded: usize,
}

/// 把 `targets` 里存在的每个存档位置打进 `zip_path`。
///
/// `now` 只用来写清单里的 `created`，由调用方传进来，测试才好断言。
pub fn pack(
    zip_path: &Path,
    targets: &[SaveTarget],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PackReport, String> {
    let mut locations = Vec::new();
    let mut missing = Vec::new();
    let mut files: Vec<(String, PathBuf, String)> = Vec::new();
    let mut excluded = 0;

    for target in targets {
        if !target.local.is_dir() {
            missing.push(target.key.clone());
            continue;
        }
        locations.push(target.key.clone());
        let (found, skipped) = collect_files(&target.local, &target.exclude)?;
        excluded += skipped;
        for (absolute, relative) in found {
            files.push((target.key.clone(), absolute, relative));
        }
    }

    // 先量一遍再写：清单要在包根，而写 zip 只能一路往前写，回头补不了。
    let mut entries = Vec::with_capacity(files.len());
    for (key, absolute, relative) in &files {
        let meta = std::fs::metadata(absolute)
            .map_err(|e| format!("读取 {} 失败: {e}", absolute.display()))?;
        entries.push(Entry {
            key: key.clone(),
            path: relative.clone(),
            size: meta.len(),
            mtime_ms: mtime_ms(&meta),
        });
    }

    let file = File::create(zip_path)
        .map_err(|e| format!("无法创建存档包 {}: {e}", zip_path.display()))?;
    let mut writer = ZipWriter::new(file);
    // deflate：Windows 资源管理器双击就能打开，压缩率也够。加密在 zip 这条路上
    // 不存在——想保护就选 kopia。
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for (key, absolute, relative) in &files {
        let name = format!("{key}/{relative}");
        writer
            .start_file(name.clone(), options)
            .map_err(|e| format!("打包 {name} 失败: {e}"))?;
        let mut source = BufReader::new(
            File::open(absolute).map_err(|e| format!("读取 {} 失败: {e}", absolute.display()))?,
        );
        std::io::copy(&mut source, &mut writer).map_err(|e| format!("写入 {name} 失败: {e}"))?;
    }

    let manifest = Manifest {
        format: FORMAT,
        created: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        locations: locations.clone(),
        entries,
    };
    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("清单序列化失败: {e}"))?;
    writer
        .start_file(MANIFEST, options)
        .map_err(|e| format!("写入清单失败: {e}"))?;
    writer
        .write_all(text.as_bytes())
        .map_err(|e| format!("写入清单失败: {e}"))?;
    writer
        .finish()
        .map_err(|e| format!("收尾存档包 {} 失败: {e}", zip_path.display()))?;

    Ok(PackReport {
        entries: manifest.entries,
        locations,
        missing,
        excluded,
    })
}

/// 递归收集一个目录下的所有普通文件，返回 `(绝对路径, 相对路径)` 与"被排除的数量"。
///
/// 不跟随符号链接：一个指回上层的软链接会让打包无限递归，而存档目录里放软链接
/// 本来也不是常见做法。相对路径一律用 `/` 分隔，跨平台进包。
pub(super) fn collect_files(
    root: &Path,
    exclude: &[String],
) -> Result<(Vec<(PathBuf, String)>, usize), String> {
    let mut found = Vec::new();
    let mut excluded = 0;
    walk(root, root, exclude, &mut found, &mut excluded)?;
    found.sort_by(|a, b| a.1.cmp(&b.1));
    Ok((found, excluded))
}

fn walk(
    root: &Path,
    dir: &Path,
    exclude: &[String],
    found: &mut Vec<(PathBuf, String)>,
    excluded: &mut usize,
) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("无法读取目录 {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取目录 {}: {e}", dir.display()))?;
        let path = entry.path();
        let relative = relative_path(root, &path);
        let file_type = entry
            .file_type()
            .map_err(|e| format!("无法识别 {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk(root, &path, exclude, found, excluded)?;
        } else if file_type.is_file() {
            if excluded_by(exclude, &relative) {
                *excluded += 1;
            } else {
                found.push((path, relative));
            }
        }
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// 这个相对路径是否被某个排除模式挡下。
///
/// 语义照 rclone 来：模式里带 `/` 时匹配整条相对路径，否则只匹配文件名
/// （所以 `*.log` 在任何一层都能挡下）。从前这条规则是交给 `rclone --exclude`
/// 的，改成一版一包之后必须由我们自己保证。
pub(super) fn excluded_by(patterns: &[String], relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        let Ok(parsed) = glob::Pattern::new(pattern) else {
            return false;
        };
        if pattern.contains('/') {
            parsed.matches_path_with(Path::new(relative), options)
        } else {
            parsed.matches_with(name, options)
        }
    })
}

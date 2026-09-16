//! 收集：把本机这一版的存档位置走一遍，攒出"要进包的东西"和"谁不在"。
//!
//! `pack`（写 zip）和 `materialize`（摆成目录）都要这一份结果，所以它单独成文件：
//! 两边唯一的区别是"装进去"那一步，收集规则必须一模一样——尤其是排除模式，
//! 从前那条规则是交给 `rclone --exclude` 的，现在得由我们自己保证两边一致。

use std::path::{Path, PathBuf};

use super::{Entry, mtime_ms};
use crate::sync::SaveTarget;

/// 一版存档收集完成后的全部事实。
pub(super) struct Gathered {
    /// 进了包的文件，按存档位置和相对路径排好序，尺寸与时间已经量好。
    pub entries: Vec<Entry>,
    /// 这一版包含哪些存档位置（目录在，哪怕空着）。
    pub locations: Vec<String>,
    /// 本机根本没有的存档位置。报告成 `skipped`，不是错误：一台机器上没装
    /// 某个位置（只在 Windows 上才有的那个目录）是正常的。
    pub missing: Vec<String>,
    /// 被排除规则挡下的文件数，只用于日志。
    pub excluded: usize,
    /// `(存档位置的 key, 绝对路径, 相对路径)`，按 key 与相对路径排好序。
    pub files: Vec<(String, PathBuf, String)>,
}

/// 走一遍所有存档位置，量好每个文件的尺寸与修改时间。
pub(super) fn gather(targets: &[SaveTarget]) -> Result<Gathered, String> {
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

    Ok(Gathered {
        entries,
        locations,
        missing,
        excluded,
        files,
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
/// （所以 `*.log` 在任何一层都能挡下）。
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

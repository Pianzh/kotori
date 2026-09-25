//! 文件系统身份与当前挂载位置。引用存盘，挂载表只作为本次解析的快照。
//! Linux 探测单独实现，其余平台仍能读取引用并明确报告不支持。

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPath {
    /// 文件系统 UUID，保留系统原始格式；不是设备名、label 或 mount ID。
    pub disk: String,
    /// 相对于文件系统根的路径，包含 bind mount / subvolume 的 root。
    pub relative: PathBuf,
}

impl MountPath {
    pub fn validate(&self) -> Result<(), String> {
        if self.disk.is_empty() || self.disk.contains(['/', '\\']) {
            return Err("挂载引用的 UUID 无效".into());
        }
        if self
            .relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("挂载引用必须使用不含 .. 的相对路径".into());
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<PathBuf, String> {
        MountTable::read().resolve(self)
    }
}

#[derive(Debug, Clone)]
pub(super) struct Mount {
    disk: Option<String>,
    device: String,
    root: PathBuf,
    point: PathBuf,
}

#[derive(Debug, Default)]
pub struct MountTable {
    entries: Vec<Mount>,
}

impl MountTable {
    pub fn read() -> Self {
        #[cfg(target_os = "linux")]
        {
            linux::read()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::default()
        }
    }

    pub fn infer(&self, path: &Path) -> Option<MountPath> {
        // 只迁移能确认实际归属的路径，旧挂载点已失效时绝不猜测。
        let path = std::fs::canonicalize(path).ok()?;
        self.infer_canonical(&path)
    }

    fn infer_canonical(&self, path: &Path) -> Option<MountPath> {
        let reference = self.reference_of(path)?;
        // 同一文件系统可能挂在多处（root 相同或不同）：解析结果未必还是原
        // 字符串，但无论落到哪个挂载点，反解出来必须还是同一个引用 —— 证明它
        // 指的就是这一份数据，而不是猜错盘。
        let resolved = self.resolve(&reference).ok()?;
        (self.reference_of(&resolved) == Some(reference.clone())).then_some(reference)
    }

    /// 把路径归到覆盖它的最长挂载点，生成盘引用（不校验往返一致性）。
    fn reference_of(&self, path: &Path) -> Option<MountPath> {
        // 未识别 UUID 的嵌套挂载也必须参与最长匹配，不能误归到父分区。
        let entry = self
            .entries
            .iter()
            .filter(|m| path.starts_with(&m.point))
            .max_by_key(|m| m.point.components().count())?;
        let relative = entry
            .root
            .strip_prefix("/")
            .ok()?
            .join(path.strip_prefix(&entry.point).ok()?);
        let reference = MountPath {
            disk: entry.disk.clone()?,
            relative,
        };
        reference.validate().ok()?;
        Some(reference)
    }

    pub fn resolve(&self, reference: &MountPath) -> Result<PathBuf, String> {
        reference.validate()?;
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|m| m.disk.as_deref() == Some(reference.disk.as_str()))
            .collect();
        let Some(first) = entries.first() else {
            return Err(format!("文件系统 {} 未挂载或无法识别", reference.disk));
        };
        if entries.iter().any(|m| m.device != first.device) {
            return Err(format!(
                "多个设备使用相同 UUID {}，请重新选择磁盘",
                reference.disk
            ));
        }
        let inside = Path::new("/").join(&reference.relative);
        let mut candidates = Vec::new();
        for entry in entries {
            let Ok(tail) = inside.strip_prefix(&entry.root) else {
                continue;
            };
            let candidate = entry.point.join(tail);
            // 路径可能被另一块盘的嵌套挂载覆盖；存在本身不能证明盘身份。
            let visible = self
                .entries
                .iter()
                .filter(|m| candidate.starts_with(&m.point))
                .max_by_key(|m| m.point.components().count());
            if let Some(visible) = visible {
                let actual = visible
                    .root
                    .join(candidate.strip_prefix(&visible.point).unwrap());
                if visible.device == entry.device && actual == inside {
                    candidates.push(candidate);
                }
            }
        }
        candidates.sort_by(|a, b| {
            a.components()
                .count()
                .cmp(&b.components().count())
                .then(a.cmp(b))
        });
        candidates.into_iter().next().ok_or_else(|| {
            format!(
                "文件系统 {} 上的目录当前不可访问: {}",
                reference.disk,
                reference.relative.display()
            )
        })
    }
}

//! Linux 挂载表与 UUID 探测。处理 mountinfo 转义和 root，设备号只用于当前快照。

use std::collections::HashMap;
use std::os::unix::{ffi::OsStringExt, fs::MetadataExt};
use std::path::PathBuf;

use super::{Mount, MountTable};

pub(super) fn read() -> MountTable {
    let mut devices = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/dev/disk/by-uuid") {
        for entry in entries.flatten() {
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                devices.insert(
                    meta.rdev(),
                    entry.file_name().to_string_lossy().into_owned(),
                );
            }
        }
    }
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return MountTable::default();
    };
    parse(&text, |device, source| {
        let (major, minor) = device.split_once(':')?;
        let major: u64 = major.parse().ok()?;
        let minor: u64 = minor.parse().ok()?;
        let dev = (minor & 0xff)
            | ((major & 0xfff) << 8)
            | ((minor & !0xff) << 12)
            | ((major & !0xfff) << 32);
        // ntfs-3g 等 FUSE 挂载的 major:minor 是虚拟设备号，回查 source 块设备。
        devices.get(&dev).cloned().or_else(|| {
            let meta = std::fs::metadata(source).ok()?;
            devices.get(&meta.rdev()).cloned()
        })
    })
}

pub(super) fn parse(text: &str, uuid: impl Fn(&str, &PathBuf) -> Option<String>) -> MountTable {
    let entries = text
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let fields: Vec<_> = left.split_whitespace().collect();
            let after: Vec<_> = right.split_whitespace().collect();
            let device = *fields.get(2)?;
            let root = decode(fields.get(3)?)?;
            let point = decode(fields.get(4)?)?;
            if !root.is_absolute() || !point.is_absolute() {
                return None;
            }
            let source = decode(after.get(1)?)?;
            Some(Mount {
                disk: uuid(device, &source),
                device: device.into(),
                root,
                point,
            })
        })
        .collect();
    MountTable { entries }
}

fn decode(text: &str) -> Option<PathBuf> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let digits = bytes.get(i + 1..i + 4)?;
            if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            let value = (digits[0] - b'0') as u16 * 64
                + (digits[1] - b'0') as u16 * 8
                + (digits[2] - b'0') as u16;
            result.push(u8::try_from(value).ok()?);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Some(std::ffi::OsString::from_vec(result).into())
}

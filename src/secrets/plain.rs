//! 明文凭据文件:权限 0600 的 JSON。
//!
//! 2026-09-13 用户拍板:**这是默认的落点**。本机没有运行中的密钥环时,凭据写在一个只有
//! 你自己能读的文件里 —— 和日常那些开发工具(opencode 的 `auth.json`、gh 的 `hosts.yml`)
//! 一模一样的做法:零输入、能跨重启,保护完全交给文件权限。
//!
//! 想更严的两条路都还在,而且都是**可选**的:
//!   * `secrets/encrypted.rs` —— 主密码加密文件(Argon2id + ChaCha20-Poly1305);
//!   * 系统密钥环 —— 有就用,没有就不强求(见 `mod.rs` 的 `open_default`)。
//!
//! 规则(每一条都有理由,别删):
//!   * **权限必须是 0600**:创建时就带上(`OpenOptions::mode`),不是先建 0644 再 chmod;
//!     读到更宽的权限就地收紧并记一条警告 —— 这和密钥环的"装了 ≠ 在跑"是同一类诚实。
//!   * **写入是原子的**:同目录临时文件 + `rename`,断电不会留下半个凭据文件。
//!   * **坏文件不当"没存过"**:解析失败要报错,否则用户以为凭据丢了,跑去重填一遍。
//!   * 文件里只有我们自己的条目(按 `SecretKey::account()`),永远不碰 `config.toml`。

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{SecretError, SecretKey};

/// 明文凭据文件。字段与用法刻意和 [`super::EncryptedFile`] 对齐,调用方不用记两套。
#[derive(Debug, Clone)]
pub struct PlainFile {
    path: PathBuf,
}

impl PlainFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// 读出全部条目。文件不存在 = 空(这是正常的"还没有存过"),不是错误。
    pub fn load(&self) -> Result<Vec<(SecretKey, String)>, SecretError> {
        if !self.exists() {
            return Ok(Vec::new());
        }
        self.tighten_permissions()?;
        let raw = fs::read_to_string(&self.path)
            .map_err(|e| SecretError::Io(format!("读取 {} 失败: {e}", self.path.display())))?;
        let map: BTreeMap<String, String> = serde_json::from_str(&raw).map_err(|e| {
            // 坏文件必须响亮地失败:说成"没存过"会让用户重填,而旧凭据其实还在。
            SecretError::Io(format!("{} 不是有效的凭据文件: {e}", self.path.display()))
        })?;
        Ok(SecretKey::ALL
            .into_iter()
            .filter_map(|key| map.get(key.account()).map(|value| (key, value.to_string())))
            .collect())
    }

    /// 原子写回,并保证 0600。
    pub fn store(&self, entries: &[(SecretKey, String)]) -> Result<(), SecretError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| SecretError::Io(format!("创建 {} 失败: {e}", dir.display())))?;
        }
        let map: BTreeMap<&str, &str> = entries
            .iter()
            .map(|(key, value)| (key.account(), value.as_str()))
            .collect();
        let body = serde_json::to_string_pretty(&map)
            .map_err(|e| SecretError::Io(format!("序列化凭据失败: {e}")))?;

        let temp = temp_path(&self.path);
        {
            let mut file = create_private(&temp)
                .map_err(|e| SecretError::Io(format!("创建 {} 失败: {e}", temp.display())))?;
            file.write_all(body.as_bytes())
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|e| SecretError::Io(format!("写入 {} 失败: {e}", temp.display())))?;
        }
        fs::rename(&temp, &self.path).map_err(|e| {
            let _ = fs::remove_file(&temp);
            SecretError::Io(format!("替换 {} 失败: {e}", self.path.display()))
        })
    }

    /// 删掉整个文件(退出明文存储时用)。
    pub fn remove(&self) -> Result<(), SecretError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretError::Io(format!(
                "删除 {} 失败: {e}",
                self.path.display()
            ))),
        }
    }

    /// 权限比 0600 宽就地收紧。返回 `true` 表示确实改过(调用方可以据此记日志)。
    pub fn tighten_permissions(&self) -> Result<bool, SecretError> {
        tighten(&self.path)
    }
}

/// 同目录的临时文件:`rename` 只有在同一个文件系统内才是原子的。
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".new.{}", std::process::id()));
    path.with_file_name(name)
}

/// 创建时就带 0600 —— 先建后 chmod 会留下一个短暂的窗口。
#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(unix)]
fn tighten(path: &Path) -> Result<bool, SecretError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)
        .map_err(|e| SecretError::Io(format!("读取 {} 属性失败: {e}", path.display())))?;
    if metadata.permissions().mode() & 0o077 == 0 {
        return Ok(false);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| {
        SecretError::Io(format!(
            "{} 的权限太宽且改不动(需要 0600): {e}",
            path.display()
        ))
    })?;
    tracing::warn!("{} 的权限比 0600 宽，已收紧", path.display());
    Ok(true)
}

/// Windows/其它平台:没有 unix 权限位可管,交给 `%APPDATA%` 的用户 ACL
/// (那正是 Windows 上"只有你能读"的等价物)。
#[cfg(not(unix))]
fn tighten(_path: &Path) -> Result<bool, SecretError> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kotori-plain-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("credentials.json")
    }

    #[test]
    fn a_round_trip_keeps_every_value_exactly() {
        let path = temp("roundtrip");
        let file = PlainFile::new(&path);
        assert!(!file.exists(), "还没写过的时候不该有文件");
        assert!(file.load().unwrap().is_empty());

        file.store(&[
            (SecretKey::B2KeyId, "005keyid".to_string()),
            (SecretKey::B2AppKey, "K005appkey".to_string()),
        ])
        .unwrap();
        assert!(file.exists());

        let mut entries = file.load().unwrap();
        entries.sort_by_key(|(key, _)| key.account());
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|(k, v)| *k == SecretKey::B2KeyId && v == "005keyid")
        );
        assert!(
            entries
                .iter()
                .any(|(k, v)| *k == SecretKey::B2AppKey && v == "K005appkey")
        );

        // 覆盖写:同一把 key 不该出现两次。
        file.store(&[(SecretKey::B2KeyId, "005other".to_string())])
            .unwrap();
        let entries = file.load().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "005other");

        file.remove().unwrap();
        assert!(!file.exists());
        // 再删一次是幂等的。
        file.remove().unwrap();
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp("mode");
        let file = PlainFile::new(&path);
        file.store(&[(SecretKey::SyncPassword, "hunter2hunter2".to_string())])
            .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "凭据文件必须是 0600，实际 {mode:o}");
        // 临时文件不能留在磁盘上。
        assert!(!temp_path(&path).exists(), "临时文件应当被 rename 掉");

        // 有人(或别的工具)把它改宽了:下次读就地收紧,并如实说改过。
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(file.tighten_permissions().unwrap());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            !file.tighten_permissions().unwrap(),
            "已经是 0600 就不该再动"
        );

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_an_empty_store() {
        let path = temp("corrupt");
        fs::write(&path, b"{ this is not json").unwrap();
        let file = PlainFile::new(&path);
        let error = file.load().unwrap_err().to_string();
        assert!(error.contains("不是有效的凭据文件"), "{error}");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

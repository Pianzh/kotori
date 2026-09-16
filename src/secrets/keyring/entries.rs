//! 一条条秘密的读写：`get` / `set` / `clear` / `present`，以及对 `secret-tool`
//! 的那一次调用。
//!
//! 每个方法都先看"背后是哪一级存储"——密钥环走子进程、明文与主密码文件走
//! `plain` / `encrypted`、内存那一级什么都不落盘。

use super::{Backend, Keyring};
use crate::secrets::{SERVICE, SecretError, SecretKey};

impl Keyring {
    /// Read a secret. `Ok(None)` means "not stored".
    pub fn get(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                return Ok(store.get(&key).cloned());
            }
            Backend::PlainFile(file) => {
                return Ok(file
                    .load()?
                    .into_iter()
                    .find(|(stored, _)| *stored == key)
                    .map(|(_, value)| value));
            }
            Backend::EncryptedFile(file) => {
                // A locked file is an error, not an empty result: the caller
                // has to be able to tell "unlock me" from "nothing here".
                return Ok(file
                    .load()?
                    .into_iter()
                    .find(|(stored, _)| *stored == key)
                    .map(|(_, value)| value));
            }
            Backend::Tool(_) => {}
        }

        let output = self.run(
            &["lookup", "service", SERVICE, "account", key.account()],
            None,
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                // secret-tool exits non-zero when the entry does not exist.
                return Ok(None);
            }
            // A broken backend must not look like "nothing stored": that sends
            // the user off to re-enter credentials that were never the problem.
            return Err(SecretError::from_stderr(&stderr));
        }
        let value = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        Ok(if value.is_empty() { None } else { Some(value) })
    }

    /// Store a secret, replacing any previous value.
    ///
    /// The value goes in over stdin so it never appears in `ps`.
    pub fn set(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let mut store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                store.insert(key, value.to_string());
                return Ok(());
            }
            Backend::PlainFile(file) => {
                let mut entries = file.load()?;
                match entries.iter_mut().find(|(stored, _)| *stored == key) {
                    Some(slot) => slot.1 = value.to_string(),
                    None => entries.push((key, value.to_string())),
                }
                return file.store(&entries);
            }
            Backend::EncryptedFile(file) => {
                let mut entries = file.load()?;
                match entries.iter_mut().find(|(stored, _)| *stored == key) {
                    Some(slot) => slot.1 = value.to_string(),
                    None => entries.push((key, value.to_string())),
                }
                return file.store(&entries);
            }
            Backend::Tool(_) => {}
        }

        let label = format!("--label={}", key.label());
        let output = self.run(
            &[
                "store",
                &label,
                "service",
                SERVICE,
                "account",
                key.account(),
            ],
            Some(value),
        )?;
        if !output.status.success() {
            return Err(SecretError::from_stderr(&String::from_utf8_lossy(
                &output.stderr,
            )));
        }
        Ok(())
    }

    /// Remove a secret. Succeeds when it was not there in the first place.
    pub fn clear(&self, key: SecretKey) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Memory(store) => {
                let mut store = store
                    .lock()
                    .map_err(|_| SecretError::Command("内存存储已损坏".to_string()))?;
                store.remove(&key);
                return Ok(());
            }
            Backend::PlainFile(file) => {
                let mut entries = file.load()?;
                entries.retain(|(stored, _)| *stored != key);
                return file.store(&entries);
            }
            Backend::EncryptedFile(file) => {
                let mut entries = file.load()?;
                entries.retain(|(stored, _)| *stored != key);
                return file.store(&entries);
            }
            Backend::Tool(_) => {}
        }

        let output = self.run(
            &["clear", "service", SERVICE, "account", key.account()],
            None,
        )?;
        if !output.status.success() {
            return Err(SecretError::Command(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }

    /// Which of our secrets exist. Used by the settings page.
    pub fn present(&self) -> Vec<SecretKey> {
        SecretKey::ALL
            .into_iter()
            .filter(|key| matches!(self.get(*key), Ok(Some(_))))
            .collect()
    }
}

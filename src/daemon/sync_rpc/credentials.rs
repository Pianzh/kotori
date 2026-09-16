//! 凭据存储本身的那几个 RPC：解锁 / 锁定 / 设主密码 / 删掉主密码文件。
//!
//! 与 `actions.rs` 分开：那边是"把存档搬来搬去"，这边是"凭据放在哪、能不能打开"，
//! 两者的失败方式完全不同（一个是网络，一个是密码）。

use serde_json::{Value, json};

use super::{Daemon, Password};
use crate::secrets::{EncryptedFile, Keyring, SecretKey};

impl Daemon {
    /// Unlock the master-password file with the password the user just typed.
    pub(in crate::daemon) fn rpc_sync_unlock(&self, password: Password) -> Result<Value, String> {
        let keyring = self.sync.keyring();
        let Some(file) = keyring.encrypted_store() else {
            // 如实报现在用的是哪一级:说"存在系统密钥环"在只跑内存的机器上是假话。
            return Err(format!(
                "当前不需要解锁（凭据存在{}里）",
                keyring.describe()
            ));
        };
        file.unlock(&password.password).map_err(|e| e.to_string())?;
        tracing::info!("凭据文件已解锁");
        Ok(json!({ "unlocked": true, "store": keyring.kind() }))
    }

    /// Move whatever credentials we have into a master-password file.
    ///
    /// This is the escape hatch for machines with no OS keyring: without it,
    /// every restart would ask for the B2 keys again, which is exactly what
    /// makes unattended sync impossible on those systems.
    pub(in crate::daemon) fn rpc_sync_set_master_password(
        &self,
        password: Password,
    ) -> Result<Value, String> {
        let path = self.sync.secrets_path().to_path_buf();
        let existing = EncryptedFile::new(&path);
        if existing.exists() && !password.force {
            return Err(
                "已经有一个主密码凭据文件了；重设主密码会重新加密它（旧密码立即失效），确认请再点一次"
                    .to_string(),
            );
        }

        let current = self.sync.keyring();
        let entries: Vec<(SecretKey, String)> = SecretKey::ALL
            .into_iter()
            .filter_map(|key| match current.get(key) {
                Ok(Some(value)) => Some((key, value)),
                _ => None,
            })
            .collect();

        existing
            .create(&password.password, &entries)
            .map_err(|e| e.to_string())?;
        // Adopt the very handle we just sealed: a fresh one would be locked.
        self.sync.adopt(Keyring::from_encrypted(existing));

        tracing::info!(
            "凭据已存入主密码文件 {}（{} 条）",
            path.display(),
            entries.len()
        );
        Ok(json!({
            "stored": true,
            "path": path.display().to_string(),
            "count": entries.len(),
        }))
    }

    /// Delete the master-password file.
    ///
    /// The credentials in it go with it; on a machine with no keyring that
    /// means they are gone. The UI asks twice.
    pub(in crate::daemon) fn rpc_sync_clear_master_password(&self) -> Result<Value, String> {
        let path = self.sync.secrets_path().to_path_buf();
        let file = EncryptedFile::new(&path);
        if !file.exists() {
            return Err("没有主密码凭据文件".to_string());
        }
        file.remove().map_err(|e| e.to_string())?;

        let fresh = Keyring::open_default(&path, self.sync.plain_path());
        self.sync.adopt(fresh);
        tracing::warn!("主密码凭据文件已删除: {}", path.display());
        Ok(json!({ "removed": true }))
    }

    /// Forget the key until the password is entered again.
    pub(in crate::daemon) fn rpc_sync_lock(&self) -> Result<Value, String> {
        let keyring = self.sync.keyring();
        let Some(file) = keyring.encrypted_store() else {
            return Err("当前不是主密码凭据文件模式".to_string());
        };
        file.lock();
        Ok(json!({ "locked": true, "store": keyring.kind() }))
    }
}

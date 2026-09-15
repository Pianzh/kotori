//! 同步设置与凭据的校验：**在动任何数据之前**把不能开始的配置挡下来。
//!
//! 单独成文件，是因为这里只回答"能不能开始"：不构造 rclone 参数
//! （见 `rclone_args`），不拼远端路径（见 `remote_paths`），也不读密钥环里的值，
//! 只判断缺了什么、哪个值一定用不了。

use super::SyncError;
use crate::config::SyncConfig;

/// Validate the settings that must be present before anything is attempted.
///
/// The only thing a B2 setup really needs is the bucket name: credentials live
/// in the keyring and rclone discovers the API host itself.
pub fn validate(settings: &SyncConfig) -> Result<(), SyncError> {
    if !settings.enabled {
        return Err(SyncError::NotEnabled);
    }
    if settings.bucket.trim().is_empty() {
        return Err(SyncError::Config("还没有填写 bucket".to_string()));
    }
    validate_endpoint(&settings.endpoint)?;
    Ok(())
}

/// Check the optional endpoint override.
///
/// This exists because the value the B2 console shows most prominently — the
/// `s3.<region>.backblazeb2.com` S3 endpoint — is **not** usable here: the
/// native B2 backend speaks the B2 API, and rclone would POST to
/// `https://s3.<region>.backblazeb2.com/b2api/...`, which does not exist.
/// Failing with that explanation beats a mystery 404 on the user's first sync.
pub fn validate_endpoint(endpoint: &str) -> Result<(), SyncError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Ok(());
    }
    if endpoint.contains("backblazeb2.com") && endpoint.trim_start().starts_with("s3.") {
        return Err(SyncError::Config(format!(
            "{endpoint} 是 B2 的 S3 兼容接口地址，原生 B2 后端用不上——把它留空即可，\n\n             rclone 会自己找到正确的 API 地址。这个字段只在需要指定特定区域端点时才填，\n             而且要写完整（含 https://）"
        )));
    }
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(SyncError::Config(format!(
            "endpoint 要写完整地址（含 https://），或者直接留空（推荐）：{endpoint}"
        )));
    }
    Ok(())
}

/// Check the secrets a run needs. Kept separate from [`validate`] so the
/// structural checks stay pure (and testable without a keyring).
pub fn validate_secrets(
    settings: &SyncConfig,
    keyring: &crate::secrets::Keyring,
) -> Result<(), SyncError> {
    // A locked store is not an empty one. Saying "no credentials yet" here
    // would send the user to re-enter keys that are already on disk.
    if let crate::secrets::StoreKind::EncryptedFile { locked: true, path } = keyring.kind() {
        return Err(SyncError::Config(format!(
            "凭据文件 {path} 已锁定，请先用主密码解锁"
        )));
    }

    let missing = |key: crate::secrets::SecretKey| !matches!(keyring.get(key), Ok(Some(_)));

    if missing(crate::secrets::SecretKey::B2KeyId) || missing(crate::secrets::SecretKey::B2AppKey) {
        return Err(SyncError::Config(
            "密钥环里还没有 B2 凭据，请先在设置页里填写".to_string(),
        ));
    }

    if settings.encryption {
        if missing(crate::secrets::SecretKey::SyncPassword) {
            return Err(SyncError::Config(
                "开启了加密，但密钥环里还没有同步密码".to_string(),
            ));
        }
        if missing(crate::secrets::SecretKey::SyncPasswordObscured) {
            return Err(SyncError::Config(
                "同步密码缺少 rclone 需要的形态，请重新保存一次密码".to_string(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::testing::settings;

    #[test]
    fn refuses_incomplete_settings() {
        let mut config = settings();
        config.enabled = false;
        assert!(matches!(validate(&config), Err(SyncError::NotEnabled)));

        let mut config = settings();
        config.bucket = String::new();
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("bucket")
        );

        // The endpoint is optional: a plain B2 setup needs only a bucket.
        let mut config = settings();
        config.endpoint = String::new();
        assert!(validate(&config).is_ok());

        // But a value that the native B2 backend cannot use is refused, with an
        // explanation — the S3 endpoint is what the B2 console shows first, and
        // sending it to the B2 API only produces a mystery 404.
        let mut config = settings();
        config.endpoint = "s3.us-west-004.backblazeb2.com".to_string();
        let error = validate(&config).unwrap_err().to_string();
        assert!(error.contains("S3 兼容接口"), "{error}");
        assert!(error.contains("留空"), "{error}");

        // A bare host is not a URL either; rclone would not add a scheme.
        let mut config = settings();
        config.endpoint = "api001.backblazeb2.com".to_string();
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("https://")
        );

        let mut config = settings();
        config.endpoint = "https://api001.backblazeb2.com".to_string();
        assert!(validate(&config).is_ok());

        // Enabling encryption is a structural setting; whether the password
        // exists is checked against the keyring by `validate_secrets`.
        let mut config = settings();
        config.encryption = true;
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn missing_secrets_are_reported_before_a_run() {
        use crate::secrets::testing::FakeTool;
        use crate::secrets::{Keyring, SecretKey};

        let fake = FakeTool::new("sync-secrets");
        let keyring: Keyring = fake.keyring();
        let config = settings();

        let error = validate_secrets(&config, &keyring).unwrap_err();
        assert!(error.to_string().contains("B2 凭据"), "{error}");

        keyring.set(SecretKey::B2KeyId, "id").unwrap();
        keyring.set(SecretKey::B2AppKey, "key").unwrap();
        assert!(validate_secrets(&config, &keyring).is_ok());

        // Encryption additionally needs the password, in both forms.
        let mut encrypted = config;
        encrypted.encryption = true;
        let error = validate_secrets(&encrypted, &keyring).unwrap_err();
        assert!(error.to_string().contains("同步密码"), "{error}");

        keyring.set(SecretKey::SyncPassword, "hunter2").unwrap();
        let error = validate_secrets(&encrypted, &keyring).unwrap_err();
        assert!(error.to_string().contains("rclone"), "{error}");

        keyring
            .set(SecretKey::SyncPasswordObscured, "obscured")
            .unwrap();
        assert!(validate_secrets(&encrypted, &keyring).is_ok());
    }
}

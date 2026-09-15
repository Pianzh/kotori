//! rclone 的运行环境：凭据怎么交给子进程，以及 rclone 可执行文件在哪。
//!
//! 单独成文件，是因为这里是"秘密只走环境变量、绝不落盘"这条规则的唯一落点
//! （ADR-010）：参数构造在 `rclone_args`，真正跑进程在 `runner`。

use super::{DEFAULT_PASSWORD2, REMOTE, null_config_path};
use crate::config::SyncConfig;

/// Environment passed to every rclone invocation.
///
/// Credentials are handed over through the child's environment (readable only by
/// its owner) and rclone is pointed at a throwaway config path, so **no secret
/// ever lands on disk** — not in `config.toml`, not in an `rclone.conf`. The
/// values themselves come from the OS keyring.
pub fn rclone_env(
    settings: &SyncConfig,
    key_id: &str,
    app_key: &str,
    obscured_password: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![
        // Ignore any rclone.conf on the machine, including the user's own.
        ("RCLONE_CONFIG".to_string(), null_config_path().to_string()),
        ("RCLONE_CONFIG_KOTORI_TYPE".to_string(), "b2".to_string()),
        (
            "RCLONE_CONFIG_KOTORI_ACCOUNT".to_string(),
            key_id.to_string(),
        ),
        ("RCLONE_CONFIG_KOTORI_KEY".to_string(), app_key.to_string()),
    ];
    if !settings.endpoint.trim().is_empty() {
        env.push((
            "RCLONE_CONFIG_KOTORI_ENDPOINT".to_string(),
            settings.endpoint.trim().to_string(),
        ));
    }
    // Note: no region. The native B2 backend derives everything it needs from
    // the credentials, and a wrong region only produces signature errors.

    if settings.encryption {
        let bucket = settings.bucket.trim().trim_matches('/');
        let prefix = settings.prefix.trim().trim_matches('/');
        let target = match (bucket.is_empty(), prefix.is_empty()) {
            (true, _) => prefix.to_string(),
            (false, true) => bucket.to_string(),
            (false, false) => format!("{bucket}/{prefix}"),
        };
        env.push((
            "RCLONE_CONFIG_KOTORIENC_TYPE".to_string(),
            "crypt".to_string(),
        ));
        env.push((
            "RCLONE_CONFIG_KOTORIENC_REMOTE".to_string(),
            format!("{REMOTE}:{target}"),
        ));
        if let Some(password) = obscured_password {
            env.push((
                "RCLONE_CONFIG_KOTORIENC_PASSWORD".to_string(),
                password.to_string(),
            ));
        }
        // A constant second factor keeps the derived key stable across machines.
        env.push((
            "RCLONE_CONFIG_KOTORIENC_PASSWORD2".to_string(),
            DEFAULT_PASSWORD2.to_string(),
        ));
        env.push((
            "RCLONE_CONFIG_KOTORIENC_FILENAME_ENCRYPTION".to_string(),
            "standard".to_string(),
        ));
        env.push((
            "RCLONE_CONFIG_KOTORIENC_DIRECTORY_NAME_ENCRYPTION".to_string(),
            "true".to_string(),
        ));
    }

    env
}

/// Is a usable rclone available?
pub fn find_rclone() -> Option<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("KOTORI_RCLONE") {
        let path = std::path::PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    crate::util::executor::find_binary("rclone")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::testing::settings;

    #[test]
    fn credentials_travel_in_the_environment_not_a_file() {
        let env = rclone_env(&settings(), "keyid123", "appkey456", None);
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };

        // Any rclone.conf on the machine is ignored, so nothing we write can
        // leak credentials.
        assert_eq!(get("RCLONE_CONFIG"), Some("/dev/null"));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_TYPE"), Some("b2"));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_ACCOUNT"), Some("keyid123"));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_KEY"), Some("appkey456"));
        // Nothing pinned: rclone discovers the API host from the credentials,
        // which is what makes a plain B2 setup work with no endpoint at all.
        assert_eq!(get("RCLONE_CONFIG_KOTORI_ENDPOINT"), None);
        // Unencrypted setups have no crypt remote at all.
        assert!(get("RCLONE_CONFIG_KOTORIENC_TYPE").is_none());
    }

    #[test]
    fn encrypted_setups_add_the_crypt_remote() {
        let mut config = settings();
        config.encryption = true;

        let env = rclone_env(&config, "k", "s", Some("obscured-blob"));
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };

        assert_eq!(get("RCLONE_CONFIG_KOTORIENC_TYPE"), Some("crypt"));
        assert_eq!(
            get("RCLONE_CONFIG_KOTORIENC_REMOTE"),
            Some("kotori:kotori-saves/prefix")
        );
        // Only the obscured form is ever handed over.
        assert_eq!(
            get("RCLONE_CONFIG_KOTORIENC_PASSWORD"),
            Some("obscured-blob")
        );
        assert_eq!(
            get("RCLONE_CONFIG_KOTORIENC_DIRECTORY_NAME_ENCRYPTION"),
            Some("true")
        );
    }
}

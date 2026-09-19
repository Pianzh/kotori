//! rclone 的运行环境：凭据怎么交给子进程。
//!
//! 单独成文件，是因为这里是"秘密只走环境变量、绝不落盘"这条规则的唯一落点
//! （ADR-010）：参数构造在 `rclone_args`，真正跑进程在 `runner`，而**可执行文件
//! 在哪**是另一件事（见 [`super::executables`]）。
//!
//! ⚠ 这里只有 B2 凭据。从前还叠过一层 `crypt` 远端（`kotorienc`）和一个同步
//! 密码；现在 rclone 这条路**不提供任何加密**（一版一个 zip，zip 里就是明文），
//! 想加密就用 kopia。

use super::null_config_path;
use crate::config::SyncConfig;

/// Environment passed to every rclone invocation.
///
/// Credentials are handed over through the child's environment (readable only by
/// its owner) and rclone is pointed at a throwaway config path, so **no secret
/// ever lands on disk** — not in `config.toml`, not in an `rclone.conf`. The
/// values themselves come from the OS keyring.
pub fn rclone_env(settings: &SyncConfig, key_id: &str, app_key: &str) -> Vec<(String, String)> {
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

    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::testing::settings;

    #[test]
    fn credentials_travel_in_the_environment_not_a_file() {
        let env = rclone_env(&settings(), "keyid123", "appkey456");
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };

        // Any rclone.conf on the machine is ignored, so nothing we write can
        // leak credentials. (The null path is per-platform: NUL on Windows.)
        assert_eq!(get("RCLONE_CONFIG"), Some(crate::sync::null_config_path()));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_TYPE"), Some("b2"));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_ACCOUNT"), Some("keyid123"));
        assert_eq!(get("RCLONE_CONFIG_KOTORI_KEY"), Some("appkey456"));
        // Nothing pinned: rclone discovers the API host from the credentials,
        // which is what makes a plain B2 setup work with no endpoint at all.
        assert_eq!(get("RCLONE_CONFIG_KOTORI_ENDPOINT"), None);
        // No crypt remote, ever: this engine does not encrypt (kopia does).
        assert!(get("RCLONE_CONFIG_KOTORIENC_TYPE").is_none());
    }

    #[test]
    fn an_endpoint_override_reaches_rclone_verbatim() {
        let mut config = settings();
        config.endpoint = "https://api001.backblazeb2.com".to_string();
        let env = rclone_env(&config, "k", "s");
        assert!(env.contains(&(
            "RCLONE_CONFIG_KOTORI_ENDPOINT".to_string(),
            "https://api001.backblazeb2.com".to_string()
        )));
    }
}

//! Cloud save sync (Phase 2).
//!
//! Design decisions (see AGENTS.md ADR-010):
//!   * **rclone** is the transfer engine, not kopia: encryption becomes an
//!     optional `crypt` layer instead of a hard requirement, and without it the
//!     saves live in the bucket as plain files that can be recovered with any
//!     S3 tool — no kopia, no kotori, not even rclone.
//!   * Retention is a *sliding window of version snapshots*, and it is
//!     **off by default** (`keep_versions = 0` keeps everything). Pruning only
//!     ever deletes old snapshots in the cloud; local saves are never touched.
//!   * Restores use `rclone copy`, never `rclone sync`, so an empty or broken
//!     backup can never delete a local save.
//!
//! This module builds and parses rclone invocations; it does not depend on
//! rclone being installed, which keeps it unit-testable.

use std::path::Path;

use crate::config::SyncConfig;

/// Remote name synthesised through rclone's environment configuration.
pub const REMOTE: &str = "kotori";
/// Remote name carrying the optional `crypt` layer. Kept free of characters
/// that would break the `RCLONE_CONFIG_<NAME>_<OPTION>` mapping.
pub const REMOTE_CRYPT: &str = "kotorienc";

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("云同步未启用")]
    NotEnabled,
    #[error("云同步配置不完整: {0}")]
    Config(String),
    #[error("rclone 未安装或不可执行: {0}")]
    RcloneMissing(String),
    #[error("rclone 执行失败: {0}")]
    Command(String),
}

/// Where version snapshots live, relative to the configured prefix.
pub const VERSIONS_DIR: &str = "versions";
/// Where the "current" copy of each save location lives.
pub const CURRENT_DIR: &str = "current";

/// Validate the settings that must be present before anything is attempted.
pub fn validate(settings: &SyncConfig) -> Result<(), SyncError> {
    if !settings.enabled {
        return Err(SyncError::NotEnabled);
    }
    if settings.bucket.trim().is_empty() {
        return Err(SyncError::Config("还没有填写 bucket".to_string()));
    }
    if settings.endpoint.trim().is_empty() {
        return Err(SyncError::Config(
            "还没有填写 S3 endpoint（B2 形如 s3.<region>.backblazeb2.com）".to_string(),
        ));
    }
    Ok(())
}

/// Check the secrets a run needs. Kept separate from [`validate`] so the
/// structural checks stay pure (and testable without a keyring).
pub fn validate_secrets(
    settings: &SyncConfig,
    keyring: &crate::secrets::Keyring,
) -> Result<(), SyncError> {
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

/// Does the configured remote carry the crypt layer?
pub fn remote_name(settings: &SyncConfig) -> &'static str {
    if settings.encryption {
        REMOTE_CRYPT
    } else {
        REMOTE
    }
}

/// `kotori:<bucket>/<prefix>` — the bucket is part of the remote path, which
/// keeps the environment-configured remote minimal.
pub fn remote_root(settings: &SyncConfig) -> String {
    let bucket = settings.bucket.trim().trim_matches('/');
    let prefix = settings.prefix.trim().trim_matches('/');
    let path = match (bucket.is_empty(), prefix.is_empty()) {
        (true, _) => prefix.to_string(),
        (false, true) => bucket.to_string(),
        (false, false) => format!("{bucket}/{prefix}"),
    };
    if path.is_empty() {
        format!("{}:", remote_name(settings))
    } else {
        format!("{}:{path}", remote_name(settings))
    }
}

/// Remote directory holding one game's save data.
pub fn game_remote(settings: &SyncConfig, game_id: &str) -> String {
    format!("{}/games/{game_id}", remote_root(settings))
}

/// Remote directory holding one game's version snapshots.
pub fn versions_remote(settings: &SyncConfig, game_id: &str) -> String {
    format!("{}/{VERSIONS_DIR}", game_remote(settings, game_id))
}

/// A stable, readable directory name for one save location.
///
/// Derived from the location description (not its index) so that reordering
/// the list in the UI cannot scramble what is already in the cloud.
pub fn save_key(save: &crate::config::SavePath) -> String {
    let mut key: String = save
        .path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while key.contains("__") {
        key = key.replace("__", "_");
    }
    let key = key.trim_matches('_').to_string();
    let key = if key.is_empty() {
        "save".to_string()
    } else {
        key
    };
    let kind = match save.kind {
        crate::config::SavePathKind::Windows => "win",
        crate::config::SavePathKind::Relative => "rel",
        crate::config::SavePathKind::Absolute => "abs",
    };
    // Keep it readable but bounded, and disambiguate kinds that could collide.
    let short: String = key.chars().take(48).collect();
    format!("{kind}-{short}")
}

/// Timestamp used for a version snapshot directory. Lexicographic order equals
/// chronological order, which makes pruning a simple sort.
pub fn version_stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Arguments for uploading local data into the cloud (`rclone copy`).
///
/// `copy` never deletes anything on the destination, and it is also what makes
/// a re-run after a failure cheap: only changed files move.
pub fn copy_args(source: &str, destination: &str, backup_dir: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "copy".to_string(),
        source.to_string(),
        destination.to_string(),
    ];
    args.push("--create-empty-src-dirs".to_string());
    if let Some(backup_dir) = backup_dir {
        // Replaced files are moved aside instead of being overwritten, which is
        // what gives us version history without a repository format.
        args.push("--backup-dir".to_string());
        args.push(backup_dir.to_string());
        args.push("--suffix".to_string());
        args.push(String::new());
    }
    args
}

/// Arguments for downloading cloud data into a local directory.
pub fn restore_args(source: &str, destination: &str) -> Vec<String> {
    // Deliberately `copy`, never `sync`: a bad backup must not delete saves.
    vec![
        "copy".to_string(),
        source.to_string(),
        destination.to_string(),
        "--create-empty-src-dirs".to_string(),
    ]
}

/// Arguments for listing the immediate sub-directories of a remote path.
pub fn list_dirs_args(remote: &str) -> Vec<String> {
    vec![
        "lsf".to_string(),
        "--dirs-only".to_string(),
        remote.to_string(),
    ]
}

/// Arguments for removing one remote directory (a version snapshot).
pub fn purge_args(remote: &str) -> Vec<String> {
    vec!["purge".to_string(), remote.to_string()]
}

/// Parse `rclone lsf --dirs-only` output into sorted directory names.
pub fn parse_dirs(output: &str) -> Vec<String> {
    let mut dirs: Vec<String> = output
        .lines()
        .map(|line| line.trim().trim_end_matches('/'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    dirs.sort();
    dirs
}

/// Which version snapshots should be removed to honour the sliding window.
///
/// `keep_versions == 0` means "keep everything" and always returns empty — the
/// default, because losing an old save silently is worse than using more space.
/// Only names that look like our own snapshot stamps are ever considered.
pub fn prune_plan(versions: &[String], keep_versions: u32) -> Vec<String> {
    if keep_versions == 0 {
        return Vec::new();
    }

    let mut stamps: Vec<&String> = versions
        .iter()
        .filter(|name| looks_like_stamp(name))
        .collect();
    stamps.sort();

    let keep = keep_versions as usize;
    if stamps.len() <= keep {
        return Vec::new();
    }
    stamps[..stamps.len() - keep]
        .iter()
        .map(|name| (*name).clone())
        .collect()
}

/// `20260911T101500Z` — the only names pruning is allowed to touch.
fn looks_like_stamp(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 16
        && bytes[8] == b'T'
        && bytes[15] == b'Z'
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit)
}

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

/// The fixed second factor mixed into the crypt key.
pub const DEFAULT_PASSWORD2: &str = "kotori";

/// Path that makes rclone ignore every config file. Windows spells it `NUL`,
/// and getting this wrong would silently make rclone read the user's own
/// `rclone.conf` — on a dual-boot machine with different settings per OS.
pub const fn null_config_path() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// Arguments that turn a plain password into the form rclone stores.
///
/// rclone insists on an obscured value in its configuration. kotori asks
/// rclone to do the conversion instead of implementing it: getting that
/// algorithm subtly wrong would derive a different key and leave the user
/// unable to open their own backups. This runs once, when the password is set.
pub fn obscure_args(password: &str) -> Vec<String> {
    vec!["obscure".to_string(), password.to_string()]
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

/// Human-readable upload plan, used by the UI and by `--dry-run` style tests.
pub fn describe_upload(settings: &SyncConfig, game_id: &str, source: &Path) -> String {
    format!("{} -> {}", source.display(), game_remote(settings, game_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SavePath, SavePathKind};

    fn settings() -> SyncConfig {
        SyncConfig {
            enabled: true,
            endpoint: "s3.us-west-004.backblazeb2.com".to_string(),
            bucket: "kotori-saves".to_string(),
            region: "us-west-004".to_string(),
            prefix: "prefix".to_string(),
            encryption: false,
            keep_versions: 0,
        }
    }

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

        let mut config = settings();
        config.endpoint = String::new();
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("endpoint")
        );

        // Enabling encryption is a structural setting; whether the password
        // exists is checked against the keyring by `validate_secrets`.
        let mut config = settings();
        config.encryption = true;
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn remote_paths_are_built_from_the_prefix() {
        let config = settings();
        // The bucket is part of the remote path, so the environment-configured
        // remote stays minimal.
        assert_eq!(remote_root(&config), "kotori:kotori-saves/prefix");
        assert_eq!(
            game_remote(&config, "3days"),
            "kotori:kotori-saves/prefix/games/3days"
        );
        assert_eq!(
            versions_remote(&config, "3days"),
            "kotori:kotori-saves/prefix/games/3days/versions"
        );

        // Encrypted setups read through the crypt remote.
        let mut encrypted = config.clone();
        encrypted.encryption = true;
        assert!(game_remote(&encrypted, "3days").starts_with("kotorienc:"));

        // An empty prefix stays valid, and so does an unset bucket.
        let mut bare = config.clone();
        bare.prefix = String::new();
        assert_eq!(remote_root(&bare), "kotori:kotori-saves");
        let mut bucketless = config;
        bucketless.bucket = String::new();
        assert_eq!(remote_root(&bucketless), "kotori:prefix");
    }

    #[test]
    fn upload_uses_copy_and_moves_replaced_files_aside() {
        let args = copy_args(
            "/saves/3days",
            "kotori:prefix/games/3days/current/win-appdata",
            Some("kotori:prefix/games/3days/versions/20260911T101500Z"),
        );
        assert_eq!(args[0], "copy", "never sync: it could delete remote data");
        assert!(args.contains(&"--backup-dir".to_string()));
        assert!(args.contains(&"kotori:prefix/games/3days/versions/20260911T101500Z".to_string()));
    }

    #[test]
    fn restore_never_deletes_local_saves() {
        let args = restore_args("kotori:prefix/games/3days/current", "/saves/3days");
        assert_eq!(args[0], "copy");
        assert!(
            !args.iter().any(|a| a == "sync"),
            "a broken backup must not be able to wipe local saves"
        );
        assert!(!args.iter().any(|a| a == "--delete" || a == "--backup-dir"));
    }

    #[test]
    fn parses_directory_listings() {
        let output = "20260911T101500Z/\n20260910T090000Z/\n\n";
        assert_eq!(
            parse_dirs(output),
            vec![
                "20260910T090000Z".to_string(),
                "20260911T101500Z".to_string()
            ]
        );
        assert!(parse_dirs("").is_empty());
    }

    #[test]
    fn retention_keeps_everything_by_default() {
        let versions: Vec<String> = (0..10).map(|i| format!("2026090{i}T000000Z")).collect();
        // 0 == keep everything: losing an old save is worse than using space.
        assert!(prune_plan(&versions, 0).is_empty());
        // Fewer versions than the window: nothing to do.
        assert!(prune_plan(&versions, 10).is_empty());
        assert!(prune_plan(&versions, 99).is_empty());
    }

    #[test]
    fn retention_removes_only_the_oldest_snapshots() {
        let versions = vec![
            "20260903T000000Z".to_string(),
            "20260901T000000Z".to_string(),
            "20260902T000000Z".to_string(),
        ];
        assert_eq!(
            prune_plan(&versions, 2),
            vec!["20260901T000000Z".to_string()],
            "the oldest snapshot goes first"
        );
        assert_eq!(
            prune_plan(&versions, 1),
            vec![
                "20260901T000000Z".to_string(),
                "20260902T000000Z".to_string()
            ]
        );
    }

    #[test]
    fn retention_ignores_anything_that_is_not_a_snapshot() {
        // A stray directory in the bucket must never be deleted by pruning.
        let versions = vec![
            "20260901T000000Z".to_string(),
            "20260902T000000Z".to_string(),
            "important-do-not-touch".to_string(),
            "current".to_string(),
        ];
        assert_eq!(
            prune_plan(&versions, 1),
            vec!["20260901T000000Z".to_string()]
        );
    }

    #[test]
    fn save_keys_are_stable_and_readable() {
        let windows = SavePath::new(SavePathKind::Windows, "%APPDATA%\\Game\\save");
        let relative = SavePath::new(SavePathKind::Relative, "savedata");
        assert_eq!(save_key(&windows), "win-appdata_game_save");
        assert_eq!(save_key(&relative), "rel-savedata");

        // Two locations that differ only by kind must not collide.
        let absolute = SavePath::new(SavePathKind::Absolute, "savedata");
        assert_ne!(save_key(&relative), save_key(&absolute));

        // Long paths are bounded; empty paths still produce a name.
        let long = SavePath::new(SavePathKind::Relative, "a".repeat(200).as_str());
        assert!(save_key(&long).len() <= 52);
        assert_eq!(
            save_key(&SavePath::new(SavePathKind::Relative, "///")),
            "rel-save"
        );
    }

    #[test]
    fn version_stamps_sort_chronologically() {
        let first = version_stamp(
            chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let second = version_stamp(
            chrono::DateTime::parse_from_rfc3339("2026-09-11T10:16:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(first, "20260911T101500Z");
        assert!(first < second, "lexicographic order must match time order");
        assert!(looks_like_stamp(&first));
    }

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
        assert_eq!(
            get("RCLONE_CONFIG_KOTORI_ENDPOINT"),
            Some("s3.us-west-004.backblazeb2.com")
        );
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

    #[test]
    fn obscuring_is_left_to_rclone() {
        // kotori must never implement this algorithm itself: a mismatch would
        // derive a different key and lock the user out of their own backups.
        assert_eq!(
            obscure_args("hunter2"),
            vec!["obscure".to_string(), "hunter2".to_string()]
        );
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

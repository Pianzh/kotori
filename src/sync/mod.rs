//! Cloud save sync (Phase 2).
//!
//! Design decisions (see HANDOVER.md ADR-010):
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

use std::path::PathBuf;

use crate::config::{Config, GameConfig, SyncConfig};

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

pub mod runner;

/// Where version snapshots live, relative to the configured prefix.
pub const VERSIONS_DIR: &str = "versions";
/// Where the "current" copy of each save location lives.
pub const CURRENT_DIR: &str = "current";

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

/// Timestamp used for a version snapshot directory.
///
/// The first 16 characters are second-precision UTC, so lexicographic order
/// equals chronological order and pruning stays a simple sort. The suffix is
/// random and is what makes the name **unique**: two uploads inside the same
/// second (a game exiting while the user hits "sync now", or the safety
/// snapshot a restore takes) would otherwise share a directory, and the later
/// one would silently destroy the earlier snapshot.
pub fn version_stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    let unique: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    format!("{}-{unique}", now.format("%Y%m%dT%H%M%SZ"))
}

/// How a transfer treats a file that already exists at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Merge {
    /// Overwrite the destination, moving whatever it replaced into the version
    /// snapshot. This is what an upload does.
    Replace,
    /// Never overwrite a **newer** file at the destination (`rclone --update`).
    ///
    /// This is what the automatic pre-launch pull uses: if an earlier upload
    /// failed (no network, machine crashed) the local saves are newer than the
    /// cloud, and a plain restore would happily throw away the progress the
    /// user just made. With `--update` the newer local copy simply wins.
    Newer,
}

/// Arguments for uploading local data into the cloud (`rclone copy`).
///
/// `copy` never deletes anything on the destination, and it is also what makes
/// a re-run after a failure cheap: only changed files move.
pub fn copy_args(
    source: &str,
    destination: &str,
    backup_dir: Option<&str>,
    merge: Merge,
) -> Vec<String> {
    let mut args = vec![
        "copy".to_string(),
        source.to_string(),
        destination.to_string(),
    ];
    args.push("--create-empty-src-dirs".to_string());
    if merge == Merge::Newer {
        args.push("--update".to_string());
    }
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

/// Append the per-location ignore patterns.
pub fn push_excludes(args: &mut Vec<String>, exclude: &[String]) {
    for pattern in exclude {
        let pattern = pattern.trim();
        if !pattern.is_empty() {
            args.push("--exclude".to_string());
            args.push(pattern.to_string());
        }
    }
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

    let mut stamps: Vec<&String> = versions.iter().filter(|name| is_snapshot(name)).collect();
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

/// `20260911T101500Z` or `20260911T101500Z-1a2b3c4d` — the only names pruning
/// is allowed to touch.
///
/// Anything else in the bucket belongs to the user or to another tool, and is
/// never deleted.
pub fn is_snapshot(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 16 {
        return false;
    }
    let base = &bytes[..16];
    if !(base[8] == b'T'
        && base[15] == b'Z'
        && base[..8].iter().all(u8::is_ascii_digit)
        && base[9..15].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let rest = &name[16..];
    rest.is_empty()
        || (rest.starts_with('-')
            && rest.len() > 1
            && rest[1..].chars().all(|c| c.is_ascii_alphanumeric()))
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

/// One configured save location, resolved to a real directory on this machine.
///
/// This is the bridge between the portable description stored in the config
/// (`%APPDATA%\Game\save`, `savedata`, `/home/me/...`) and something rclone can
/// be pointed at. The `key` is derived from the description, not the index, so
/// reordering the list in the UI cannot scramble what is already in the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveTarget {
    pub key: String,
    /// The configured location, for user-facing messages.
    pub configured: String,
    /// Where it lives here and now.
    pub local: PathBuf,
    /// Glob patterns rclone must skip.
    pub exclude: Vec<String>,
}

/// Resolve every save location of a game into a local directory.
///
/// Fails loudly rather than silently syncing the wrong thing: a location that
/// cannot be resolved, or that resolves to the filesystem root (which would
/// mean "upload the whole disk"), aborts the whole game.
pub fn targets(game: &GameConfig, config: &Config) -> Result<Vec<SaveTarget>, String> {
    let (root, _) = crate::wine::SaveRoot::for_platform(game, config);
    let game_dir = game.effective_game_dir();

    let mut targets = Vec::with_capacity(game.save_paths.len());
    for save in &game.save_paths {
        let local = crate::wine::resolve_save_path(&root, &game_dir, save)?;
        if local.as_os_str().is_empty() {
            return Err(format!("存档位置「{}」解析为空路径", save.path));
        }
        // `/` has no parent; nothing legitimate about syncing a whole disk.
        if local.parent().is_none() {
            return Err(format!(
                "存档位置「{}」解析到了文件系统根目录，拒绝同步",
                save.path
            ));
        }
        targets.push(SaveTarget {
            key: save_key(save),
            configured: save.path.clone(),
            local,
            exclude: save.exclude.clone(),
        });
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SavePath, SavePathKind};

    fn settings() -> SyncConfig {
        SyncConfig {
            enabled: true,
            endpoint: String::new(),
            bucket: "kotori-saves".to_string(),
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
            Merge::Replace,
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
        assert!(first.starts_with("20260911T101500Z"), "{first}");
        assert!(first < second, "lexicographic order must match time order");
        assert!(is_snapshot(&first), "{first}");
        assert!(is_snapshot("20260911T101500Z"));
        assert!(!is_snapshot("20260911T101500"), "no Z");
        assert!(!is_snapshot("20260911T101500Z_extra"), "only -suffix");
        assert!(!is_snapshot("20260911X101500Z"), "T separator is required");
        assert!(!is_snapshot("not-a-stamp"));
        assert!(!is_snapshot(""));
    }

    #[test]
    fn two_uploads_in_the_same_second_do_not_share_a_snapshot() {
        // Regression: snapshot directories used to be named to the second, so
        // the second upload overwrote the first one's history — and a restore's
        // safety snapshot could destroy the very snapshot being restored.
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-11T10:15:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let first = version_stamp(now);
        let second = version_stamp(now);
        assert_ne!(first, second);
        for stamp in [first, second] {
            assert!(is_snapshot(&stamp), "{stamp}");
            assert!(stamp.starts_with("20260911T101500Z-"), "{stamp}");
        }
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

    #[test]
    fn save_locations_resolve_to_this_machines_directories() {
        use crate::config::{GameConfig, SavePath, ScaleProfile};

        let dir = std::env::temp_dir().join(format!(
            "kotori-sync-targets-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let prefix = dir.join("prefix");
        let user = prefix.join("drive_c/users/tester");
        std::fs::create_dir_all(&user).unwrap();

        let mut config = Config::default();
        config.wine.prefix = Some(prefix.clone());
        let game = GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            save_paths: vec![
                SavePath::inferred("savedata"),
                SavePath::inferred("%APPDATA%\\Demo\\save"),
            ],
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        };

        let resolved = targets(&game, &config).unwrap();
        assert_eq!(resolved.len(), 2);
        // The cloud key comes from the *description*, so it is the same on
        // Windows and on wine (ADR-008) — and cannot be scrambled by reordering.
        assert_eq!(resolved[0].key, "rel-savedata");
        assert_eq!(resolved[0].local, PathBuf::from("/games/demo/savedata"));
        assert_eq!(resolved[1].key, "win-appdata_demo_save");
        assert_eq!(
            resolved[1].local,
            user.join("AppData/Roaming/Demo/save"),
            "the token resolves inside the wine prefix"
        );

        // Without a prefix the token still resolves (to the default ~/.wine).
        let bare = targets(&game, &Config::default()).unwrap();
        assert!(bare[1].local.ends_with("AppData/Roaming/Demo/save"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn syncing_the_filesystem_root_is_refused() {
        use crate::config::{GameConfig, SavePath, SavePathKind, ScaleProfile};

        let game = GameConfig {
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            // A typo here would mean "upload the whole disk".
            save_paths: vec![SavePath::new(SavePathKind::Absolute, "/")],
            wine_prefix: None,
            watch_only: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        };

        let error = targets(&game, &Config::default()).unwrap_err();
        assert!(error.contains("根目录"), "{error}");
    }
}

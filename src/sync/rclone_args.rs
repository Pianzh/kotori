//! 一次 rclone 调用的参数表：上行与下行都用 `copy`，列目录用 `lsf`，删快照用
//! `purge`，外加把明文密码交给 rclone 自己 `obscure`。
//!
//! 单独成文件，是因为"参数长什么样"（这里）、"凭据从哪来"（`rclone_env`）、
//! "跑起来以后怎么解读结果"（`runner`）是三件事。这里全是纯函数：不碰进程、
//! 不碰网络，可以逐条断言。

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

/// Arguments that turn a plain password into the form rclone stores.
///
/// rclone insists on an obscured value in its configuration. kotori asks
/// rclone to do the conversion instead of implementing it: getting that
/// algorithm subtly wrong would derive a different key and leave the user
/// unable to open their own backups. This runs once, when the password is set.
pub fn obscure_args(password: &str) -> Vec<String> {
    vec!["obscure".to_string(), password.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn obscuring_is_left_to_rclone() {
        // kotori must never implement this algorithm itself: a mismatch would
        // derive a different key and lock the user out of their own backups.
        assert_eq!(
            obscure_args("hunter2"),
            vec!["obscure".to_string(), "hunter2".to_string()]
        );
    }
}

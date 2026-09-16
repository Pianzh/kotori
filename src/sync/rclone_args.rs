//! 一次 rclone 调用的参数表：一个包上行或下行都用 `copyto`（整份搬一个对象），
//! 列包用 `lsf --files-only`，删旧包用 `deletefile`。
//!
//! 单独成文件，是因为"参数长什么样"（这里）、"凭据从哪来"（`rclone_env`）、
//! "跑起来以后怎么解读结果"（`runner`）是三件事。这里全是纯函数：不碰进程、
//! 不碰网络，可以逐条断言。
//!
//! ⚠ 这里**没有** `copy`/`sync` 了：一版一包之后，一次传输就是一个 zip 对象的
//! 一来一回，不存在"目录对目录地合并"这件事——合并（只取新的 / 覆盖）改由我们
//! 自己在 `archive` 里做，那是 ADR-012 的新落点。

/// Arguments for moving one local file to a remote object (`rclone copyto`).
///
/// `copyto` 而不是 `copy`：源是一个临时 zip 文件，目的地是一个**确切的远端
/// 对象名**，不是目录。B2 的对象上传是原子的（没传完的对象不在列表里），所以
/// 不需要 `.part` + rename 那一套。
pub fn copyto_args(source: &str, destination: &str) -> Vec<String> {
    vec![
        "copyto".to_string(),
        source.to_string(),
        destination.to_string(),
    ]
}

/// Arguments for listing the files (not directories) of a remote path.
///
/// 远端目录里只有包，所以 `lsf` 就够了：不需要递归，也不需要传输大小。
pub fn list_files_args(remote: &str) -> Vec<String> {
    vec![
        "lsf".to_string(),
        "--files-only".to_string(),
        remote.to_string(),
    ]
}

/// Arguments for removing one remote object (an expired version package).
pub fn deletefile_args(remote: &str) -> Vec<String> {
    vec!["deletefile".to_string(), remote.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transfer_is_one_object_each_way() {
        let args = copyto_args("/tmp/kotori-3days.zip", "kotori:prefix/games/3days/v.zip");
        assert_eq!(args[0], "copyto", "源是文件、目的地是确切的对象名");
        assert_eq!(args.len(), 3);
        // 目录式的合并已经不存在了：它正是"快照只是差量"那套东西的入口。
        for forbidden in ["copy", "sync", "--backup-dir", "--update"] {
            assert!(!args.iter().any(|a| a == forbidden), "{args:?}");
        }
    }

    #[test]
    fn listing_asks_for_files_only_and_deleting_names_one_object() {
        assert_eq!(
            list_files_args("kotori:prefix/games/3days"),
            vec![
                "lsf".to_string(),
                "--files-only".to_string(),
                "kotori:prefix/games/3days".to_string()
            ]
        );
        assert_eq!(
            deletefile_args("kotori:prefix/games/3days/v.zip"),
            vec![
                "deletefile".to_string(),
                "kotori:prefix/games/3days/v.zip".to_string()
            ]
        );
    }
}

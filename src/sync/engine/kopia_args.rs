//! kopia 那一边的参数表与输出解析：全是纯函数，不碰进程、不碰网络。
//!
//! 单独成文件，是因为"参数长什么样"与"跑起来以后怎么解读结果"（`kopia.rs`）
//! 是两件事，前者可以逐条断言。
//!
//! ## 秘密怎么走
//!
//! 仓库密码（[`crate::secrets::SecretKey::KopiaPassword`]）走 **`KOPIA_PASSWORD`
//! 环境变量**，绝不进 argv。
//!
//! ⚠ **B2 的 `--key-id` / `--key` 只能进 argv**：kopia 0.22 的 b2 provider 把这两个
//! 声明成 required flag，没有环境变量绑定（实测：只给 `KOPIA_B2_KEY_ID`/`KOPIA_B2_KEY`
//! 会报 `required flag(s) '--key', '--key-id' not provided`）。所以这里如实照办，
//! 但**只在建立仓库那一次**：`repository connect` 会把凭据落到 `KOPIA_CONFIG_PATH`
//! 指向的配置文件里，之后每一次快照、恢复、删除都只靠那份配置加环境变量里的密码。
//! 连上之后再也不会有一条命令行带着 B2 key。

use serde::Deserialize;

use crate::config::SyncConfig;

/// 仓库在桶里的前缀。
///
/// 与 rclone 那条路的 `<prefix>/games/<id>/` **不重叠**：两个引擎各写各的区域，
/// 换引擎不会把对方的对象当成自己的版本读进来。
pub fn repo_prefix(settings: &SyncConfig) -> String {
    let prefix = settings.prefix.trim().trim_matches('/');
    if prefix.is_empty() {
        "kopia".to_string()
    } else {
        format!("{prefix}/kopia")
    }
}

/// 建一个新仓库（桶里还没有的时候）。
pub(super) fn create_args(settings: &SyncConfig, key_id: &str, key: &str) -> Vec<String> {
    let mut args = vec![
        "repository".to_string(),
        "create".to_string(),
        "b2".to_string(),
    ];
    args.extend(repository_flags(settings, key_id, key));
    args
}

/// 连上一个已经存在的仓库（换机器、或者重装之后）。
pub(super) fn connect_args(settings: &SyncConfig, key_id: &str, key: &str) -> Vec<String> {
    let mut args = vec![
        "repository".to_string(),
        "connect".to_string(),
        "b2".to_string(),
    ];
    args.extend(repository_flags(settings, key_id, key));
    args
}

/// 本地目录仓库（`KOTORI_KOPIA_REPOSITORY`）。
///
/// 存在的理由有两个：验收测试要能在**不碰 B2、不联网**的前提下跑完整的
/// 上传→取回→回退闭环；以及有人就是想把仓库放在挂载进来的 NAS 目录上。
/// 这不是一条独立的"引擎"——布局、快照、版本名与 B2 完全一样，只有仓库落在哪不同。
pub(super) fn create_filesystem_args(path: &str) -> Vec<String> {
    vec![
        "repository".to_string(),
        "create".to_string(),
        "filesystem".to_string(),
        format!("--path={path}"),
    ]
}

pub(super) fn connect_filesystem_args(path: &str) -> Vec<String> {
    vec![
        "repository".to_string(),
        "connect".to_string(),
        "filesystem".to_string(),
        format!("--path={path}"),
    ]
}

fn repository_flags(settings: &SyncConfig, key_id: &str, key: &str) -> Vec<String> {
    vec![
        format!("--bucket={}", settings.bucket.trim()),
        format!("--key-id={key_id}"),
        format!("--key={key}"),
        format!("--prefix={}", repo_prefix(settings)),
    ]
}

/// 拍一版快照。
///
/// `description` 存的是我们自己的版本名：两个引擎因此共用同一套版本标识，
/// "最新的一版"和"保留最近 N 版"在上层不用分叉。`game:` 标签是列快照时的筛子
/// ——快照的 source 是**本机绝对路径**，双系统/多机上根本对不上，不能用它认游戏。
pub(super) fn snapshot_create_args(game_id: &str, stamp: &str, source: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "create".to_string(),
        "--json".to_string(),
        format!("--tags=game:{game_id}"),
        format!("--description={stamp}"),
        source.to_string(),
    ]
}

/// 列出某个游戏的全部快照。
pub(super) fn snapshot_list_args(game_id: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "list".to_string(),
        "--json".to_string(),
        format!("--tags=game:{game_id}"),
    ]
}

/// 删掉一个快照。
///
/// ⚠ `--delete` 不能省：kopia 的 `snapshot delete` **默认是演练**
/// （"Would delete snapshot … (pass --delete to confirm)"），少写它就等于什么都没删，
/// 而保留窗口会安静地失效。
pub(super) fn snapshot_delete_args(id: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "delete".to_string(),
        "--delete".to_string(),
        id.to_string(),
    ]
}

/// 把一个快照恢复到一个目录。
pub(super) fn restore_args(id: &str, into: &str) -> Vec<String> {
    vec!["restore".to_string(), id.to_string(), into.to_string()]
}

/// `snapshot list --json` 里的一条。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Snapshot {
    pub id: String,
    /// 我们自己写进去的版本名；不是 kotori 建的快照这里是空的。
    #[serde(default)]
    pub description: String,
}

/// 解析 `snapshot list --json`，只留下**我们自己建的**那些，按版本名排序。
///
/// 判据与 rclone 那条路同一套（[`super::super::is_snapshot`]）：描述不像我们写的
/// 版本名就绝不碰——用户可能拿同一个 kopia 仓库放着别的东西。
pub(super) fn parse_snapshots(text: &str) -> Result<Vec<Snapshot>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut ours: Vec<Snapshot> = all
        .into_iter()
        .filter(|snapshot| super::super::is_snapshot(&snapshot.description))
        .collect();
    ours.sort_by(|a, b| a.description.cmp(&b.description));
    Ok(ours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::testing::settings;

    fn settings_with_prefix(prefix: &str) -> SyncConfig {
        SyncConfig {
            prefix: prefix.to_string(),
            ..settings()
        }
    }

    #[test]
    fn the_repository_lives_beside_the_rclone_packages_never_on_top_of_them() {
        assert_eq!(repo_prefix(&settings()), "prefix/kopia");
        // 空的 prefix 就是桶根：rclone 那条路放在 `games/`，两条路仍然错开。
        assert_eq!(repo_prefix(&settings_with_prefix("")), "kopia");
        assert_eq!(repo_prefix(&settings_with_prefix("/")), "kopia");
        assert_eq!(repo_prefix(&settings_with_prefix("/deep/")), "deep/kopia");
    }

    #[test]
    fn creating_and_connecting_differ_only_in_the_subcommand() {
        let settings = settings();
        let create = create_args(&settings, "kid", "secret");
        let connect = connect_args(&settings, "kid", "secret");
        assert_eq!(&create[..3], ["repository", "create", "b2"]);
        assert_eq!(&connect[..3], ["repository", "connect", "b2"]);
        assert_eq!(create[3..], connect[3..]);

        assert!(create.contains(&"--bucket=kotori-saves".to_string()));
        assert!(create.contains(&"--prefix=prefix/kopia".to_string()));
        // B2 的 key 只能这样传（kopia 没有环境变量绑定），所以这里如实断言：
        // 它确实在 argv 上——这是记录在案的妥协，不是漏掉的一步。
        assert!(create.contains(&"--key-id=kid".to_string()));
        assert!(create.contains(&"--key=secret".to_string()));
        // 密码绝不在这里：它走 KOPIA_PASSWORD。
        assert!(!create.iter().any(|arg| arg.contains("--password")));
    }

    #[test]
    fn a_snapshot_is_marked_with_our_own_version_name_and_a_game_tag() {
        let args = snapshot_create_args("3days", "20260916T120000000Z-abcd1234", "/tmp/payload");
        assert_eq!(&args[..2], ["snapshot", "create"]);
        // --json 才有干净的 stdout（进度条走 stderr）。
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--tags=game:3days".to_string()));
        assert!(args.contains(&"--description=20260916T120000000Z-abcd1234".to_string()));
        assert_eq!(args.last().unwrap(), "/tmp/payload");
    }

    #[test]
    fn deleting_a_snapshot_says_it_out_loud() {
        // kopia 的 delete 默认只演练：漏了 --delete 就是"保留窗口静默失效"。
        let args = snapshot_delete_args("abc123");
        assert_eq!(args, ["snapshot", "delete", "--delete", "abc123"]);
    }

    #[test]
    fn only_snapshots_we_named_ourselves_are_ours() {
        let json = r#"[
          {"id":"aaa","description":"20260916T120000000Z-abcd1234","startTime":"2026-09-16T12:00:00Z"},
          {"id":"bbb","description":"","startTime":"2026-09-16T13:00:00Z"},
          {"id":"ccc","description":"my own backup","startTime":"2026-09-16T14:00:00Z"},
          {"id":"ddd","description":"20260915T100000Z","startTime":"2026-09-15T10:00:00Z"}
        ]"#;
        let ours = parse_snapshots(json).unwrap();
        // 空的、以及别人随手写的描述都不算；老格式（16 位）仍然认。
        assert_eq!(
            ours.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["ddd", "aaa"],
            "按版本名排序，最旧在前"
        );
    }

    #[test]
    fn broken_json_is_reported_not_swallowed() {
        assert!(parse_snapshots("not json").is_err());
    }
}

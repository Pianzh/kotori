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

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::SyncConfig;
use crate::sync::cloud::{CloudGame, IDENTITY_DESCRIPTION, KIND_IDENTITY, KIND_SAVE};
use crate::sync::is_snapshot;

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
///
/// `kind=save` 也是筛子：同一个仓库里还放着**身份快照**（见
/// [`identity_snapshot_args`]），`versions()` 与保留窗口只许看存档那一种（§5.3）。
pub(super) fn snapshot_create_args(game_id: &str, stamp: &str, source: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "create".to_string(),
        "--json".to_string(),
        format!("--tags=game:{game_id}"),
        format!("--tags=kind:{KIND_SAVE}"),
        format!("--description={stamp}"),
        source.to_string(),
    ]
}

/// 拍一条**身份快照**：源目录里只有那份 `kotori-game.json`。
///
/// kopia 的仓库是它自己的私有格式，没有"每款游戏一个目录"可以摆身份卡，所以身份也
/// 只能靠快照。两条约束：
///   * `description` 是一句**固定的话**，绝不能长得像版本名 —— 否则它会被
///     [`parse_snapshots`] 当成一版存档，进而被保留窗口删掉（§5.3）；
///   * `kind=identity` 是它自己的筛子（列身份、以及"哪些不是存档"都靠它）。
pub(super) fn identity_snapshot_args(cloud_id: &str, source: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "create".to_string(),
        "--json".to_string(),
        format!("--tags=game:{cloud_id}"),
        format!("--tags=kind:{KIND_IDENTITY}"),
        format!("--description={IDENTITY_DESCRIPTION}"),
        source.to_string(),
    ]
}

/// 列出某个游戏的全部快照。
///
/// ⚠ **`-a`（`--all`）不能省**：kopia 的 `snapshot list` 默认只列"当前用户名 /
/// 当前主机名"拍的快照，而快照的 source 在双系统、双机上是**各写各的**。少了它，
/// 另一台机器传上去的版本在这台机器上一条都列不出来 —— 现象是"这个游戏云端没有
/// 版本"这种**静默的谎**（上传其实是成功的），而 `versions` / `latest` / `restore`
/// 共用这条读路径。
///
/// 2026-09-22 本机实测（本地 filesystem 仓库、两份 kopia 配置当两台机器）：0.22.3
/// 上不带 `-a` 也能列出别的 source 的快照（带 `<source>` 参数那条路才看得出区别），
/// 所以这个标志今天是**保险**而不是"修好了一个正在犯的错"。它问的是"把所有机器的
/// 快照都列出来"，正是这条路径想要的语义。
pub(super) fn snapshot_list_args(game_id: &str) -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "list".to_string(),
        "-a".to_string(),
        "--json".to_string(),
        format!("--tags=game:{game_id}"),
    ]
}

/// 列出仓库里**所有**我们自己的快照（不按游戏筛）。
///
/// "云端有哪几款游戏"只能这样问：游戏名是写在标签里的，问之前还不知道该填什么。
pub(super) fn snapshot_list_all_args() -> Vec<String> {
    vec![
        "snapshot".to_string(),
        "list".to_string(),
        "-a".to_string(),
        "--json".to_string(),
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
    /// 快照上的标签。
    ///
    /// ⚠ 实测（kopia 0.22.3）：JSON 里的键带 `tag:` 前缀 —— `--tags=game:3days`
    /// 打出来的是 `{"tag:game":"3days"}`。读标签请走 [`Snapshot::tag`]。
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// 快照开始时刻（kopia 的 RFC3339，纳秒精度）。
    ///
    /// 身份快照的描述是固定的一句话，所以"最新的那条"只能靠时间认（见
    /// [`identity_snapshots`]）。定长零填充的 RFC3339 字符串可以直接比大小。
    #[serde(default, rename = "startTime")]
    pub start_time: String,
}

impl Snapshot {
    /// 读一个标签，带不带 `tag:` 前缀都认（前缀是 kopia 的输出细节，不是我们的语义）。
    pub(super) fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .get(key)
            .or_else(|| self.tags.get(&format!("tag:{key}")))
            .map(String::as_str)
    }

    /// 这一条是不是我们自己拍的。
    ///
    /// 两条判据任一成立即可：描述长成我们自己的版本名（存档快照），或者带着
    /// `kind` 标签（身份快照的描述是一句固定的话，本来就不该长得像版本名）。
    fn is_ours(&self) -> bool {
        is_snapshot(&self.description) || self.tag("kind").is_some()
    }

    /// 这一条是身份快照吗（不是"一版存档"）。
    fn is_identity(&self) -> bool {
        self.tag("kind") == Some(KIND_IDENTITY)
    }
}

/// 按 `game:` 标签把仓库里的快照归到各自的游戏名下。
///
/// 这就是"云端有哪几款游戏"：kopia 的仓库是一个不透明的大块，能认人的只有标签。
/// 没带 `game:` 标签、也不是我们拍的快照一律不看 —— 用户可能拿同一个仓库放着别的
/// 备份，那些东西不该出现在游戏的列表里。
pub(super) fn parse_cloud_games(text: &str) -> Result<Vec<CloudGame>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut games: BTreeMap<String, CloudGame> = BTreeMap::new();
    for snapshot in all {
        if !snapshot.is_ours() {
            continue;
        }
        let Some(id) = snapshot.tag("game") else {
            continue;
        };
        let game = games.entry(id.to_string()).or_insert_with(|| CloudGame {
            id: id.to_string(),
            versions: 0,
        });
        // 身份快照不是"一版存档"：它只有一句话，没有存档内容。
        if is_snapshot(&snapshot.description) {
            game.versions += 1;
        }
    }
    Ok(games.into_values().collect())
}

/// 每个云端身份最新的那条**身份快照**：`(cloud_id, 快照 id)`。
///
/// 同一个身份每次上传都会再拍一条（快照不可变，改不了旧的），所以这里按时间取最新
/// 的一条。读它的内容还要一次 `kopia restore`（§5.4：一次读 = 起一个进程），
/// 所以调用方要缓存。
pub(super) fn identity_snapshots(text: &str) -> Result<Vec<(String, String)>, String> {
    let all: Vec<Snapshot> =
        serde_json::from_str(text).map_err(|e| format!("读不懂 kopia 的快照列表: {e}"))?;
    let mut newest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snapshot in all {
        if !snapshot.is_identity() {
            continue;
        }
        let Some(cloud_id) = snapshot.tag("game") else {
            continue;
        };
        match newest.get(cloud_id) {
            Some(known) if known.start_time >= snapshot.start_time => {}
            _ => {
                newest.insert(cloud_id.to_string(), snapshot);
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(cloud_id, snapshot)| (cloud_id, snapshot.id))
        .collect())
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
        .filter(|snapshot| {
            // 描述得像版本名，而且**不是**身份快照：两种快照躺在同一个仓库里，
            // 版本列表与保留窗口只许看存档那一种（§5.3）。
            super::super::is_snapshot(&snapshot.description) && !snapshot.is_identity()
        })
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
    fn listing_asks_for_every_machine_not_just_this_one() {
        // ⚠ 少了 `-a`，另一台机器传上去的版本会一条都列不出来（"云端没有版本"
        // 这种静默的谎）。这条把它钉住：这条读路径永远带上 `-a`。
        let args = snapshot_list_args("3days");
        assert_eq!(&args[..3], ["snapshot", "list", "-a"]);
        assert!(args.contains(&"--tags=game:3days".to_string()));
        assert!(args.contains(&"--json".to_string()));

        // "云端有哪几款游戏"是同一个问法的无筛选版。
        assert_eq!(
            snapshot_list_all_args(),
            vec!["snapshot", "list", "-a", "--json"]
        );
    }

    #[test]
    fn cloud_games_come_from_the_game_tag_and_identity_snapshots_are_not_versions() {
        // 实测（0.22.3）：JSON 里的标签键带 `tag:` 前缀。
        let json = r#"[
          {"id":"a","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"save"}},
          {"id":"b","description":"20260916T130000000Z-abcd1234","tags":{"tag:game":"3days"}},
          {"id":"c","description":"20260915T100000Z","tags":{"game":"life-game"}},
          {"id":"d","description":"kotori-identity","tags":{"tag:game":"life-game","tag:kind":"identity"}},
          {"id":"e","description":"someone else's backup","tags":{"tag:game":"not-ours"}},
          {"id":"f","description":"20260916T140000000Z-abcd1234","tags":{}},
          {"id":"g","description":"my own backup","tags":{}}
        ]"#;
        let games = parse_cloud_games(json).unwrap();
        assert_eq!(
            games,
            vec![
                // 两个存档快照（一个带 kind、一个不带都算），身份快照不算版本。
                CloudGame {
                    id: "3days".to_string(),
                    versions: 2
                },
                CloudGame {
                    id: "life-game".to_string(),
                    versions: 1
                },
            ],
            "按 id 排序；没有 game 标签的、以及别人拍的一律不出现"
        );
        assert!(parse_cloud_games("[]").unwrap().is_empty());
        assert!(parse_cloud_games("not json").is_err());
    }

    #[test]
    fn broken_json_is_reported_not_swallowed() {
        assert!(parse_snapshots("not json").is_err());
    }
    #[test]
    fn the_identity_snapshot_is_marked_as_identity_and_never_looks_like_a_version() {
        let args = identity_snapshot_args("cloud-1", "/tmp/identity");
        assert_eq!(&args[..2], ["snapshot", "create"]);
        assert!(args.contains(&"--tags=game:cloud-1".to_string()));
        // ⚠ kopia 的标签是 `key:value`（实测：写成 `kind=identity` 会被拒：
        // "Invalid tag format (kind=identity). Requires <key>:<value>"）。
        assert!(args.contains(&"--tags=kind:identity".to_string()));
        assert!(args.contains(&format!("--description={IDENTITY_DESCRIPTION}")));
        assert_eq!(args.last().unwrap(), "/tmp/identity");
        // ⚠ 描述绝不能长得像版本名：那它就会被当成一版存档，进而被保留窗口删掉。
        assert!(!is_snapshot(IDENTITY_DESCRIPTION), "{IDENTITY_DESCRIPTION}");

        // 存档快照那边带的是 kind=save —— 两种快照在同一个仓库里靠它分家。
        let save = snapshot_create_args("3days", "20260901T000000000Z-aaaa1111", "/tmp/payload");
        assert!(save.contains(&"--tags=kind:save".to_string()));
    }

    #[test]
    fn identity_snapshots_are_never_counted_as_versions() {
        // 即使描述被改成了版本名，`kind=identity` 也说了算：保留窗口绝不许碰它。
        let json = r#"[
              {"id":"s1","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"save"},"startTime":"2026-09-16T12:00:00Z"},
              {"id":"i1","description":"20260915T000000000Z-abcd1234","tags":{"tag:game":"3days","tag:kind":"identity"},"startTime":"2026-09-15T10:00:00Z"}
            ]"#;
        let ours = parse_snapshots(json).unwrap();
        assert_eq!(
            ours.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["s1"],
            "身份快照不是一版存档"
        );

        // 身份快照按**时间**取最新的一条（描述是固定的一句话，分不出新旧）。
        let json = r#"[
              {"id":"old","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-15T10:00:00Z"},
              {"id":"new","description":"kotori-identity","tags":{"tag:game":"c1","tag:kind":"identity"},"startTime":"2026-09-16T10:00:00Z"},
              {"id":"c2","description":"kotori-identity","tags":{"tag:game":"c2","tag:kind":"identity"},"startTime":"2026-09-14T10:00:00Z"},
              {"id":"save","description":"20260916T120000000Z-abcd1234","tags":{"tag:game":"c1","tag:kind":"save"},"startTime":"2026-09-17T10:00:00Z"}
            ]"#;
        assert_eq!(
            identity_snapshots(json).unwrap(),
            vec![
                ("c1".to_string(), "new".to_string()),
                ("c2".to_string(), "c2".to_string()),
            ],
            "每个身份取最新那条；存档快照不算身份快照"
        );
    }
}

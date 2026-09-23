//! 云端索引：**一个桶一份**的"云端现在有什么"。
//!
//! 为什么要有它（用户 2026-09-23 定的）：从前"列云端有哪些游戏、叫什么名字"要读**每一张
//! 身份卡**，而 kopia 那边读一张 = 一次 `restore` —— 云端 200 款就是 200 趟。索引把这一下
//! 变成"列一次 + 读一次"。
//!
//! **逻辑上一份，物理上"合并快照 + 未合并增量"**：
//!
//!   * `main`（[`INDEX_FILE`]）= 并集快照，里面记着它已经并过哪些增量（[`CloudIndex::merged`]）；
//!   * 每次写入**先**写一条增量（对象名唯一，谁也覆盖不了谁），**再**重写 `main`。
//!
//! 这么安排是为了并发：两台机器同时同步时，各自"读-改-写"同一份 `main` 会**互相抹掉**
//! （两边各读旧内容、各写回，后写的把先写的条目弄丢）——"云端明明有这一款、列表里没有"
//! 正是这一整件事要消灭的错。有了增量，丢掉的那一条仍然躺在 `log/` 里，读的时候一定
//! 会被并进来；崩在"写完增量、还没写 main"中间也一样。
//!
//! ⚠ **身份卡仍是真相，索引只是镜像**：卡每款一份，是索引丢了/坏了以后唯一便宜的重建
//! 来源（包里的清单要下载整个 zip 才读得到）。所以"删 / 改"一律以真实列举为准。
//!
//! 这个模块里只有数据与纯函数（怎么并、怎么命名、怎么判断该不该并），I/O 在两个引擎里。

use serde::{Deserialize, Serialize};

use super::cloud::{GameIdentity, MachineIdentity};

/// 索引的格式版本。**不认识就当读不懂**，绝不猜（索引只是加速，猜错会认错游戏）。
pub const INDEX_FORMAT: u32 = 1;
/// 合并快照的文件名（rclone 是桶里那个对象名；kopia 是快照里那个文件名）。
pub const INDEX_FILE: &str = "kotori-index.json";
/// 增量放的目录名（rclone 是 `index/log/`；kopia 是索引快照的 `index` 标签前缀）。
pub const INDEX_LOG: &str = "log";
/// 合并快照在 kopia 快照标签 `index:` 下的值。
pub const INDEX_MAIN: &str = "main";
/// rclone 那边放索引的目录名（`<prefix>/index/`）。
pub const INDEX_DIR: &str = "index";

/// 索引里的一行：云端的一款游戏。
///
/// 身份那部分**整段复用身份卡**（名字、指纹、机器、存档位置 key 都在这儿），所以索引与
/// 卡说的是同一套字段，不会各说各话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGame {
    /// 这一款在云端的落点（rclone 是目录名，kopia 是 `game:` 标签值）。
    pub cloud_key: String,
    /// 身份：名字、指纹、机器、存档位置 key。
    pub identity: GameIdentity,
    /// 有几版（很轻的摘要，见模块头那段"常变字段"的说明）。
    #[serde(default)]
    pub versions: usize,
    /// 最近一版的版本名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// 最近一版多大（字节；不知道就是 0）。
    #[serde(default)]
    pub size: u64,
    /// 这一条最后一次被写是什么时候 —— 合并时**比它**（RFC3339，UTC，等宽，字典序即时间序）。
    pub updated: String,
}

impl IndexGame {
    /// 从一张身份卡起一条。
    pub fn from_identity(cloud_key: &str, identity: GameIdentity) -> Self {
        Self {
            cloud_key: cloud_key.to_string(),
            identity,
            versions: 0,
            latest: None,
            size: 0,
            updated: now(),
        }
    }

    /// 把一台机器并进来（指纹/位置/用过的 exe 路径都只追加）。
    pub fn merge_machine(&mut self, machine: MachineIdentity) {
        self.identity.merge_machine(machine);
        self.touch();
    }

    /// 记下这一款现在的摘要（版数 / 最近一版 / 大小），并把时间戳推到现在。
    pub fn set_summary(&mut self, versions: usize, latest: Option<String>, size: u64) {
        self.versions = versions;
        self.latest = latest;
        self.size = size;
        self.touch();
    }

    fn touch(&mut self) {
        self.updated = now();
    }
}

/// 一个桶一份的索引。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudIndex {
    pub format: u32,
    /// 这一份是什么时候合成的。
    pub updated: String,
    /// 已经并进 `games` 的增量名字。读的时候靠它跳过（**清理也只许删这里面的**）。
    #[serde(default)]
    pub merged: Vec<String>,
    #[serde(default)]
    pub games: Vec<IndexGame>,
}

impl Default for CloudIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudIndex {
    pub fn new() -> Self {
        Self {
            format: INDEX_FORMAT,
            updated: now(),
            merged: Vec::new(),
            games: Vec::new(),
        }
    }

    /// 一份**增量**：只装着这台机器这次要说的那几条，`merged` 空着（它还没有并过任何人）。
    pub fn delta(games: Vec<IndexGame>) -> Self {
        Self {
            games,
            ..Self::new()
        }
    }

    /// 按 `cloud_id` 并一条进来；同一条**取 `updated` 新的**。
    ///
    /// "新的整条胜出"是安全的，因为每个写入者动手之前都先读一遍并集（见模块头）：
    /// 它写出去的自己那一条，已经是"并集 + 我这台机器"的样子了。
    pub fn merge(&mut self, game: IndexGame) {
        match self
            .games
            .iter_mut()
            .find(|known| known.identity.cloud_id == game.identity.cloud_id)
        {
            Some(known) if known.updated >= game.updated => {}
            Some(known) => *known = game,
            None => self.games.push(game),
        }
    }

    /// 把若干份增量并进来（**调用方负责先滤掉 `merged` 里已经记过的**）。
    ///
    /// 返回实际并进来的名字，写 `main` 时原样记进 `merged`。
    pub fn absorb(&mut self, deltas: Vec<(String, CloudIndex)>) -> Vec<String> {
        let mut names = Vec::new();
        for (name, delta) in deltas {
            for game in delta.games {
                self.merge(game);
            }
            names.push(name);
        }
        self.updated = now();
        names
    }

    /// 这条增量还需要读吗？（读路径就是靠它跳过已经并过的。
    ///
    /// ⚠ 清理增量时也**只许**删 [`Self::merged`] 里记着的那些 —— 没记着的一律当"还没并"。
    pub fn needs(&self, delta: &str) -> bool {
        !self.merged.iter().any(|known| known == delta)
    }

    /// 记下"这份增量已经并进 `games` 了"。
    pub fn mark_merged(&mut self, name: &str) {
        if self.needs(name) {
            self.merged.push(name.to_string());
        }
    }

    /// 索引里有几款游戏。
    pub fn len(&self) -> usize {
        self.games.len()
    }

    /// 排序稳定一点，界面与 JSON 都好读（落点在前）。
    pub fn sort(&mut self) {
        self.games.sort_by(|a, b| a.cloud_key.cmp(&b.cloud_key));
    }
}

/// 读回来的索引：合并快照 + 那些**还没并进去**的增量。
///
/// 两个引擎都返回它（rclone 是列一次目录 + 读几个对象，kopia 是列一次快照 + restore 几条），
/// 之后的合并是纯函数（[`IndexBundle::merged_view`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexBundle {
    /// 桶里的并集快照；还没有就是 `None`（第一次用、或者索引被删了）。
    pub main: Option<CloudIndex>,
    /// `(增量名, 内容)` —— **已经不在这里面的**（读的时候按 `main.merged` 滤过）。
    pub deltas: Vec<(String, CloudIndex)>,
}

impl IndexBundle {
    /// 一个桶一份的索引现在是这副样子（合并快照 + 未合并增量）。
    pub fn merged_view(&self) -> CloudIndex {
        let mut union = self.main.clone().unwrap_or_default();
        // 增量里的条目按时间戳合并，所以这里不必（也不该）记账：`merged` 是**写**的时候
        // 记的（见 `Runner::update_index`）。
        union.absorb(self.deltas.clone());
        union.sort();
        union
    }

    /// 桶里到底有没有索引（`None` 的 `main` + 空增量 = 一次都没写过）。
    pub fn is_empty(&self) -> bool {
        self.main.is_none() && self.deltas.is_empty()
    }
}

/// 增量对象的名字：`<machine_id>-<时间戳>.json`。
///
/// **必须唯一**：同名就是覆盖，而覆盖正是这套设计要避免的事。机器 id 是一次性 uuid，
/// 时间戳精确到毫秒 —— 同一台机器同一毫秒写两条增量是不可能的（一次同步只写一条）。
pub fn delta_name(machine_id: &str, at: &str) -> String {
    format!("{machine_id}-{at}.json")
}

/// 这个名字长得像不像一条增量（`<machine_id>-<时间戳>.json`）。
///
/// rclone 那边列 `index/log/` 只能拿到一串名字，所以"哪些是我们的"必须有判据 ——
/// 桶是用户的，里面可能被塞了别的东西。
pub fn is_delta_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    match stem.rsplit_once('-') {
        // 时间戳那一段必须是我们自己写出来的形状（`20260923T101500123Z`）。
        Some((machine, at)) => {
            !machine.is_empty()
                && at.len() == 19
                && at.ends_with('Z')
                && at[..15].chars().enumerate().all(|(index, c)| {
                    if index == 8 {
                        c == 'T'
                    } else {
                        c.is_ascii_digit()
                    }
                })
                && at[15..18].chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// 文件名友好的时间戳（`20260923T101500123Z`）。
///
/// 增量对象名用它：**不能带冒号** —— Windows 上冒号是非法文件名字符，而 kopia 那边还要
/// 拿这个名字当源目录名。
pub fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string()
}

/// 现在（RFC3339、UTC、秒精度、等宽）。索引里所有时间戳都走它，字典序才是时间序。
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(id: &str, prints: &[&str], paths: &[&str]) -> MachineIdentity {
        MachineIdentity {
            machine_id: id.to_string(),
            label: format!("host-{id}"),
            fingerprints: prints.iter().map(|p| p.to_string()).collect(),
            locations: vec!["rel-savedata".to_string()],
            exe_paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn game(cloud_id: &str, key: &str, name: &str, machine: MachineIdentity) -> IndexGame {
        let mut identity = GameIdentity::new(cloud_id, name);
        identity.merge_machine(machine);
        IndexGame::from_identity(key, identity)
    }

    #[test]
    fn a_delta_is_a_whole_index_without_any_merge_history() {
        let delta = CloudIndex::delta(vec![game("c1", "one", "一", machine("a", &["f1"], &[]))]);
        assert_eq!(delta.format, INDEX_FORMAT);
        assert!(delta.merged.is_empty(), "增量自己没并过谁");
        assert_eq!(delta.len(), 1);
    }

    #[test]
    fn merging_keeps_one_entry_per_identity_and_lets_the_newer_one_win() {
        let mut union = CloudIndex::new();
        let mut older = game("c1", "one", "旧名字", machine("a", &["f1"], &[]));
        older.updated = "2026-09-23T10:00:00Z".to_string();
        union.merge(older);

        // 旧的不许盖掉新的。
        let mut stale = game("c1", "one", "陈年副本", machine("b", &["f0"], &[]));
        stale.updated = "2026-09-22T10:00:00Z".to_string();
        union.merge(stale);
        assert_eq!(union.len(), 1);
        assert_eq!(
            union.games[0].identity.name, "旧名字",
            "{}",
            union.games[0].identity.name
        );

        // 新的可以。
        let mut newer = game("c1", "one", "新名字", machine("b", &["f2"], &[]));
        newer.updated = "2026-09-23T11:00:00Z".to_string();
        union.merge(newer);
        assert_eq!(union.len(), 1);
        assert_eq!(union.games[0].identity.name, "新名字");

        // 另一款就是另一条。
        union.merge(game("c2", "two", "另一款", machine("a", &["f9"], &[])));
        assert_eq!(union.len(), 2);
    }

    #[test]
    fn absorbing_deltas_reports_what_it_swallowed() {
        let mut main = CloudIndex::new();
        main.merge(game("c1", "one", "一", machine("a", &["f1"], &[])));
        let names = main.absorb(vec![
            (
                "machine-b-20260923T100000Z.json".to_string(),
                CloudIndex::delta(vec![game("c2", "two", "二", machine("b", &["f2"], &[]))]),
            ),
            // 空增量也照样记账：它确实被读过了，没必要下次再读一遍。
            (
                "machine-c-20260923T100001Z.json".to_string(),
                CloudIndex::delta(Vec::new()),
            ),
        ]);
        assert_eq!(names.len(), 2);
        assert_eq!(main.len(), 2);
        assert!(main.games.iter().any(|game| game.identity.cloud_id == "c2"));
    }

    #[test]
    fn a_bundle_merges_its_deltas_and_remembers_what_it_swallowed() {
        let mut main = CloudIndex::new();
        let mut settled = game("c1", "one", "一", machine("a", &["f1"], &[]));
        settled.updated = "2026-09-23T10:00:00Z".to_string();
        main.merge(settled);
        assert!(main.needs("machine-b-2026.json"));

        let bundle = IndexBundle {
            main: Some(main),
            deltas: vec![(
                "machine-b-2026.json".to_string(),
                CloudIndex::delta(vec![game("c2", "two", "二", machine("b", &["f2"], &[]))]),
            )],
        };
        let union = bundle.merged_view();
        assert_eq!(union.len(), 2, "增量也要算进来");
        assert!(!bundle.is_empty());

        // 并过的增量就不用再读了 —— 这条判据同时是"清理只许删这些"的依据。
        let mut after = bundle.main.clone().unwrap();
        after.mark_merged("machine-b-2026.json");
        assert!(!after.needs("machine-b-2026.json"));
        assert!(after.needs("machine-c-2026.json"), "没记着的一律当还没并");

        // 桶里一次都没写过时是"空"，区分得开"云端没有游戏"与"索引还没建"。
        assert!(IndexBundle::default().is_empty());
        assert_eq!(IndexBundle::default().merged_view().len(), 0);
    }

    #[test]
    fn every_timestamp_is_the_same_width_so_string_order_is_time_order() {
        let a = "2026-09-23T09:59:59Z";
        let b = "2026-09-23T10:00:00Z";
        assert_eq!(a.len(), b.len(), "等宽是字典序=时间序的前提");
        assert!(a < b);
        let written = now();
        assert_eq!(written.len(), a.len(), "{written}");
        assert!(written.ends_with('Z'), "{written}");
        assert_eq!(
            delta_name("1a2b", "20260923T100000Z"),
            "1a2b-20260923T100000Z.json"
        );
        // 增量名会当文件名/目录名用：不许有冒号（Windows 上非法）。
        let fresh = stamp();
        assert!(!fresh.contains(':'), "{fresh}");
        assert!(is_delta_name(&delta_name("1a2b", &fresh)), "{fresh}");
        // 不是我们写的东西一律不认（桶是用户的）。
        for bad in [
            "",
            "notes.json",
            "1a2b.json",
            "1a2b-2026.json",
            "1a2b-xxx.json",
        ] {
            assert!(!is_delta_name(bad), "{bad}");
        }
    }

    #[test]
    fn an_unknown_format_version_is_not_guessed_at() {
        let text = serde_json::to_string(&CloudIndex::new()).unwrap();
        assert_eq!(
            serde_json::from_str::<CloudIndex>(&text).unwrap().format,
            INDEX_FORMAT
        );
        // 未来版本：读得进来但格式号不认识 —— 上层据此当"读不懂"，不当"能用"。
        let future = r#"{"format":99,"updated":"2026-09-23T10:00:00Z","games":[]}"#;
        assert_ne!(
            serde_json::from_str::<CloudIndex>(future).unwrap().format,
            INDEX_FORMAT
        );
        // 少字段的索引也要读得进来（`games` / `merged` 缺了当空，不猜）。
        let sparse = r#"{"format":1,"updated":"2026-09-23T10:00:00Z"}"#;
        let parsed: CloudIndex = serde_json::from_str(sparse).unwrap();
        assert!(parsed.games.is_empty() && parsed.merged.is_empty());
        // `updated` 是结构字段，缺了就是坏文件 —— 不当成"空索引"（那会让它永远赢不了合并）。
        assert!(serde_json::from_str::<CloudIndex>(r#"{"format":1}"#).is_err());
    }
}

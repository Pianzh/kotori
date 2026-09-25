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
    /// 这一款在云端的**词条已经被删掉**（用户按的 `sync.delete_identity`）。
    ///
    /// ⚠ 它和 `versions == 0` 是两件事：版数为 0 是"这一款还在云端、只是没有存档"，
    /// 而这个说的是"云端已经不认识它了"。列表那边靠它把整条跳过 —— 索引只会增/改、
    /// 不会减，没有这个标志就删不掉一条。重新上传会清掉它（见 `Backend::update_index`）。
    #[serde(default)]
    pub gone: bool,
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
            gone: false,
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
    /// 这份索引的格式我们认不认识。
    ///
    /// 桶是**外部输入**：未来版本写下的索引不该被这一版按"当前字段"解释。三条读取
    /// 路径（rclone、kopia、本地缓存）都要过这一关 —— 认不出就当它没有，让深扫重写
    /// 一份（BUG-25）。写入路径不用问：自己写的就是自己认的格式。
    pub fn is_supported(&self) -> bool {
        self.format == INDEX_FORMAT
    }

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
            Some(known) if known.updated >= game.updated => {
                // 这一条比对面新，但对面可能带着**我们没有的机器**：两台机器同时写
                // 索引时，整条按时间胜出会把对方那台机器的指纹、位置与 exe 路径丢掉
                // （BUG-24）。机器是"一台一条"的集合，只并集、绝不覆盖。
                for machine in &game.identity.machines {
                    known.identity.merge_machine(machine.clone());
                }
            }
            Some(known) => {
                let mut incoming = game;
                for machine in &known.identity.machines {
                    incoming.identity.merge_machine(machine.clone());
                }
                *known = incoming;
            }
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

    /// 云端现在有哪些款对得上这个指纹：0 个 = 云端没有它，1 个 = 就是它，≥2 个 = **要问**。
    ///
    /// 判据与配对**同一条**（[`GameIdentity::has_fingerprint`]，见 `sync::pairing` 的
    /// 唯一命中才自动绑）：这里只是把它挪到索引上 —— 添加游戏时读一次索引就够，不必为了
    /// 认一款去遍历每一张身份卡。
    pub fn by_fingerprint<'a>(&'a self, fingerprint: &str) -> Vec<&'a IndexGame> {
        self.games
            .iter()
            .filter(|game| game.identity.has_fingerprint(fingerprint))
            .collect()
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
    let Some((machine, at)) = stem.rsplit_once('-') else {
        return false;
    };
    // 时间戳那一段必须是我们自己写出来的形状（`20260923T101500123Z`）。
    //
    // ⚠ 一律按**字节**看，不按下标切 `str`：`at` 是桶里的名字，谁都能写 —— 凑够
    // 19 个字节的多字节名字会让 `at[..15]` 落在字符中间直接 panic，一条异常对象名
    // 就能打断整个索引读取（BUG-26）。
    let bytes = at.as_bytes();
    !machine.is_empty()
        && bytes.len() == 19
        && bytes[18] == b'Z'
        && bytes[..15].iter().enumerate().all(|(index, byte)| {
            if index == 8 {
                *byte == b'T'
            } else {
                byte.is_ascii_digit()
            }
        })
        && bytes[15..18].iter().all(|byte| byte.is_ascii_digit())
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
// ⚠ 这是**文件模块**(`index.rs` 这种),它的子模块默认要放在同名目录下
// (`index/`);测试就住在同一个目录里,用 `#[path]` 指过去 —— 比为了一个
// 测试文件专门建目录清楚。
#[path = "index_tests.rs"]
mod index_tests;

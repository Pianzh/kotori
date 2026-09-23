//! 「云端存档」这一页的状态：云端有哪些游戏、点开的那一款有哪几版。
//!
//! 与 `sync`（本机的设置与凭据）分开：这一块**只读云端**，一个字都不改本机配置，而且它
//! 记的是"上次读索引时看到的样子"——云端会在别的机器上变，所以它天生是一张快照。
//!
//! 数据来源是**一个桶一份的云端索引**（`crate::sync::index`）：列这些游戏只读一次索引，
//! 不去遍历每一张身份卡。索引还没建过时（`indexed == false`）页面要提示去点一次「深度
//! 扫描云端」——那才是读所有卡的那条慢路。

/// 云端的一款游戏（`sync.cloud_list` 的一行）。
///
/// ⚠ `cloud_key` 是**云端落点**（rclone 是目录名，kopia 是 `game:` 标签值），不是本机
/// id：云端有而本机没有的游戏也要能列出来，它没有 id 可用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudGameRow {
    pub cloud_key: String,
    pub cloud_id: String,
    /// **云端记下的游戏名**（不是落点 —— 那是机器话）。
    pub name: String,
    /// 有几台机器认领过它。
    pub machines: usize,
    pub versions: usize,
    /// 最近一版的版本名与它多大（字节；0 = 不知道）。
    pub latest: Option<String>,
    pub size: u64,
    /// 这台/那台机器用过的 exe 路径：只给人看、只给搜索用。
    pub exe_paths: Vec<String>,
    /// 本机哪一条档案认了它（空 = 本机没有对应的）。
    pub local_id: String,
    pub local_name: String,
    /// 本机明确否过这一条（配对表那笔账）。
    pub rejected: bool,
}

impl CloudGameRow {
    pub(in crate::ui) fn versions_label(&self) -> String {
        match self.versions {
            0 => "还没有存档".to_string(),
            count => format!("{count} 版"),
        }
    }

    /// 最近一版那一行：时间 + 大小。两边都不知道时如实说"不知道"。
    pub(in crate::ui) fn latest_label(&self) -> String {
        let Some(latest) = &self.latest else {
            return String::new();
        };
        let time = crate::sync::describe_stamp(latest);
        match self.size {
            0 => time,
            size => format!("{time} · {}", human_size(size)),
        }
    }

    /// 本机这一侧的状态：认了哪一条 / 本机没有 / 你之前说了不是它。
    pub(in crate::ui) fn local_label(&self) -> String {
        if self.rejected {
            return "你说过不是这一款".to_string();
        }
        if self.local_id.is_empty() {
            return "本机没有它".to_string();
        }
        format!("本机《{}》", self.local_name)
    }

    /// 这一行是不是这一个（搜索用：名字、落点、用过的 exe 路径）。
    ///
    /// ⚠ `exe_paths` 只在这里被用到 —— 它**不参与任何判断**（谁是谁只看指纹），也不写回
    /// 本机配置（见 `crate::sync::cloud::MachineIdentity::exe_paths`）。
    pub(in crate::ui) fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(&needle)
            || self.cloud_key.to_lowercase().contains(&needle)
            || self
                .exe_paths
                .iter()
                .any(|path| path.to_lowercase().contains(&needle))
    }
}

/// 云端的一版存档（`sync.cloud_versions` 的一行）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudVersionRow {
    pub name: String,
    pub size: u64,
    pub time: String,
}

impl CloudVersionRow {
    /// 给人看的那一行：本机时区的时间。
    pub(in crate::ui) fn label(&self) -> String {
        if self.time.is_empty() {
            crate::sync::describe_stamp(&self.name)
        } else {
            // 引擎给的是 UTC RFC3339；换算与排版交给同一个函数，两边说法才一致。
            crate::sync::describe_stamp(&self.name)
        }
    }

    pub(in crate::ui) fn size_label(&self) -> String {
        match self.size {
            0 => "大小不知道".to_string(),
            size => human_size(size),
        }
    }
}

/// 一次"读云端清单"的结果 —— 清单本身，加上它**是从哪儿来的**。
///
/// 用户 2026-09-23 定的：索引要在本机查、界面要显示是什么时候拿到的、后台刷新失败也要
/// 让人知道。所以这三件事跟清单一起回给界面（见 `daemon::sync_rpc::index::IndexView`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudListReply {
    /// 桶里建过索引没有。`false` = 还没建过（提示去深扫一次），与"云端没有游戏"不同。
    pub indexed: bool,
    /// 这一份来自**本机缓存**（这一趟没打网络）。
    pub from_cache: bool,
    /// 什么时候拿到的（`stamp` 形状，显示时走 `describe_stamp`）。
    pub cached_at: String,
    /// 最近一次刷新失败的原因（有的话）。
    pub refresh_error: Option<String>,
    pub rows: Vec<CloudGameRow>,
}

impl CloudListReply {
    /// 「这份清单是什么时候、从哪儿来的」—— 用户要的那句时间。
    pub(in crate::ui) fn source_label(&self) -> String {
        source_label(self.from_cache, &self.cached_at)
    }

    /// 后台刷新失败那句（没有就不说）。
    pub(in crate::ui) fn trouble_label(&self) -> Option<String> {
        trouble_label(self.refresh_error.as_deref())
    }
}

/// 「本机缓存 · 2026-09-23 10:15」/「刚从云端读的 · …」；不知道时间就什么都不说。
pub(in crate::ui) fn source_label(from_cache: bool, cached_at: &str) -> String {
    if cached_at.is_empty() {
        return String::new();
    }
    let when = crate::sync::describe_stamp(cached_at);
    if from_cache {
        format!("本机缓存 · {when}")
    } else {
        format!("刚从云端读的 · {when}")
    }
}

/// 后台刷新失败那句（没有就不说）。
pub(in crate::ui) fn trouble_label(error: Option<&str>) -> Option<String> {
    error.map(|error| format!("上次刷新失败: {error}"))
}

/// 把字节数写成给人看的一行（只保留一位小数）。
pub(in crate::ui) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 「云端存档」整页的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudState {
    /// 云端有哪些游戏（未过滤）。
    pub rows: Vec<CloudGameRow>,
    /// 桶里**建过索引**没有。`false` = 还没建过（要提示去深扫一次），与"云端没有游戏"分得开。
    pub indexed: bool,
    /// 这一次运行里**读过一次**了吗（进页面自动读一次，之后要用户自己按刷新）。
    pub loaded_once: bool,
    pub loading: bool,
    /// 正在深度扫描（读所有身份卡，慢）。
    pub scanning: bool,
    /// 一句话状态（读完 / 出错）。空 = 什么都别说。
    pub msg: Option<String>,
    pub ok: bool,
    /// 搜索框里的字（本地过滤，不打网络）。
    pub search: String,
    /// 点开的那一款（云端落点）；`None` = 在列表上。
    pub open: Option<String>,
    /// 点开那一款的版本，最旧在前。
    pub versions: Vec<CloudVersionRow>,
    pub versions_loading: bool,
}

impl Default for CloudState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            indexed: false,
            loaded_once: false,
            loading: false,
            scanning: false,
            msg: None,
            // 还没出过错 —— 第一句话不该是红色。
            ok: true,
            search: String::new(),
            open: None,
            versions: Vec::new(),
            versions_loading: false,
        }
    }
}

impl CloudState {
    /// 搜索之后要显示的那些行。
    pub(in crate::ui) fn visible(&self) -> Vec<&CloudGameRow> {
        self.rows
            .iter()
            .filter(|row| row.matches(&self.search))
            .collect()
    }

    /// 云端这几款里，本机已经认了的有几款（"扫描完成"那句话要用）。
    pub(in crate::ui) fn matched(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| !row.local_id.is_empty())
            .count()
    }

    /// 点开的那一款（在列表里找不到时是 `None`：刷新之后它可能已经不在了）。
    pub(in crate::ui) fn opened(&self) -> Option<&CloudGameRow> {
        let key = self.open.as_deref()?;
        self.rows.iter().find(|row| row.cloud_key == key)
    }

    /// 点一下某一款：开着的那一款收起来，别的换成它。
    ///
    /// 返回 `true` 表示**要去云端问一次版本**——收起来不用问，这是这个函数唯一的判断。
    pub(in crate::ui) fn toggle(&mut self, key: &str) -> bool {
        if self.open.as_deref() == Some(key) {
            self.open = None;
            self.versions.clear();
            self.versions_loading = false;
            return false;
        }
        self.open = Some(key.to_string());
        self.versions.clear();
        self.versions_loading = true;
        true
    }

    /// 回到列表（详情页的「返回」）。
    pub(in crate::ui) fn back(&mut self) {
        self.open = None;
        self.versions.clear();
        self.versions_loading = false;
    }

    /// 某一款的版本回来了。
    ///
    /// ⚠ **只认还开着的那一款**：两份回包可能乱序（点开甲、又点开乙，甲的回包后到），
    /// 把甲的结果铺到乙底下就是给用户看错东西。
    pub(in crate::ui) fn versions_loaded(&mut self, key: &str, versions: Vec<CloudVersionRow>) {
        if self.open.as_deref() != Some(key) {
            return;
        }
        self.versions = versions;
        self.versions_loading = false;
    }

    /// 某一款的版本没列成。
    pub(in crate::ui) fn versions_failed(&mut self, key: &str, message: String) {
        if self.open.as_deref() != Some(key) {
            return;
        }
        self.versions.clear();
        self.versions_loading = false;
        self.msg = Some(message);
        self.ok = false;
    }

    /// 清单回来了：整张表换掉，顺手把点开的那一款收起来（它可能已经不在了）。
    ///
    /// ⚠ "这份清单什么时候拿到的"**不存在这里** —— 它属于那句话本身（`cloud_summary` 拼进
    /// `msg` 了）。这一页存的是"表里有什么"，不是"什么时候看的"。
    pub(in crate::ui) fn loaded(&mut self, reply: CloudListReply) {
        self.rows = reply.rows;
        self.indexed = reply.indexed;
        self.loaded_once = true;
        self.loading = false;
        self.open = None;
        self.versions.clear();
        self.versions_loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, name: &str, local: &str) -> CloudGameRow {
        CloudGameRow {
            cloud_key: key.to_string(),
            cloud_id: format!("id-{key}"),
            name: name.to_string(),
            machines: 2,
            versions: 3,
            latest: Some("20260911T101500Z".to_string()),
            size: 4096,
            exe_paths: vec![format!("/games/{key}/game.exe")],
            local_id: local.to_string(),
            local_name: if local.is_empty() {
                String::new()
            } else {
                "本机那一款".to_string()
            },
            rejected: false,
        }
    }

    fn board() -> CloudState {
        CloudState {
            rows: vec![
                row("demo", "示例游戏", "demo"),
                row("other", "别的一款", ""),
            ],
            indexed: true,
            ..CloudState::default()
        }
    }

    /// 「这份清单是什么时候拿到的」是用户 2026-09-23 要的那一句：缓存/刚读到两种说法，
    /// 时间按本机时区印（长度固定，与跑测试的机器无关）；时间不知道（老回包）时就不印空时间。
    #[test]
    fn the_reply_says_when_this_list_was_fetched() {
        assert!(
            source_label(true, "20260923T101500Z").starts_with("本机缓存 · "),
            "{}",
            source_label(true, "20260923T101500Z")
        );
        assert!(
            source_label(false, "20260923T101500Z").starts_with("刚从云端读的 · "),
            "{}",
            source_label(false, "20260923T101500Z")
        );
        assert_eq!(source_label(true, ""), "", "不知道时间就别说时间");
        assert_eq!(trouble_label(None), None, "上一次是好的就别说话");
        assert_eq!(
            trouble_label(Some("连不上桶")).as_deref(),
            Some("上次刷新失败: 连不上桶")
        );
    }

    #[test]
    fn a_new_board_is_quiet_and_not_red() {
        let fresh = CloudState::default();
        assert!(fresh.msg.is_none() && fresh.ok);
        assert!(!fresh.indexed && !fresh.loading && !fresh.scanning);
        assert!(fresh.visible().is_empty());
    }

    #[test]
    fn each_row_says_how_many_versions_and_where_the_machine_stands() {
        let board = board();
        assert_eq!(board.rows[0].versions_label(), "3 版");
        assert_eq!(board.rows[1].local_label(), "本机没有它");
        assert_eq!(board.rows[0].local_label(), "本机《本机那一款》");
        // 时间按本机时区印出来（长度固定，与跑测试的机器无关）。
        assert_eq!(
            board.rows[0].latest_label().len(),
            "2026-09-11 18:15 · 4.0 KiB".len()
        );

        let mut rejected = board.rows[0].clone();
        rejected.rejected = true;
        assert_eq!(rejected.local_label(), "你说过不是这一款");
        // 一版都没有时那句"最近一版"是空的，不是"不知道"。
        let mut empty = board.rows[0].clone();
        empty.versions = 0;
        empty.latest = None;
        assert_eq!(empty.versions_label(), "还没有存档");
        assert_eq!(empty.latest_label(), "");
    }

    #[test]
    fn sizes_are_written_the_way_people_read_them() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(4096), "4.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(CloudVersionRow::default().size_label(), "大小不知道");
    }

    #[test]
    fn search_matches_the_name_the_key_and_the_exe_path() {
        let mut board = board();
        // 名字、落点、exe 路径都能搜到；大小写不敏感；空串等于不过滤。
        // （两行的 exe 路径里都有 `game.exe`，所以那条要用带落点的那一段来区分。）
        for needle in ["示例", "demo", "DEMO", "demo/game.exe"] {
            board.search = needle.to_string();
            assert_eq!(board.visible().len(), 1, "{needle}");
        }
        board.search = "game.exe".to_string();
        assert_eq!(board.visible().len(), 2, "两行的 exe 都叫 game.exe");
        board.search = "  ".to_string();
        assert_eq!(board.visible().len(), 2, "空串不过滤");
        board.search = "zzz".to_string();
        assert!(board.visible().is_empty());
    }

    #[test]
    fn a_late_reply_never_lands_under_the_wrong_game() {
        let mut board = board();
        board.toggle("demo");
        board.toggle("other");
        // 甲的回包后到：必须丢掉，而不是铺到乙底下。
        board.versions_loaded("demo", vec![CloudVersionRow::default()]);
        assert!(board.versions.is_empty(), "{:?}", board.versions);
        assert!(board.versions_loading, "还在等乙的版本");

        board.versions_loaded(
            "other",
            vec![CloudVersionRow {
                name: "20260910T090000Z".into(),
                size: 128,
                time: String::new(),
            }],
        );
        assert_eq!(board.versions.len(), 1);
        assert!(!board.versions_loading);
        assert_eq!(board.versions[0].size_label(), "128 B");

        // 报错走同一条判据：别人的错不该挂在这一款上。
        board.versions_failed("demo", "列的途中断了".into());
        assert!(board.ok && board.msg.is_none());
        board.versions_failed("other", "列的途中断了".into());
        assert!(!board.ok && board.versions.is_empty());

        // 「返回」回到列表：开着的那一款与它的版本一起清掉。
        board.back();
        assert_eq!(board.open, None);
        assert!(board.opened().is_none());
    }

    #[test]
    fn refreshing_closes_whatever_was_open() {
        let mut board = board();
        board.toggle("demo");
        board.loaded(CloudListReply {
            indexed: true,
            rows: vec![row("demo", "示例游戏", "demo")],
            ..CloudListReply::default()
        });
        assert!(board.indexed && !board.loading);
        assert_eq!(board.open, None, "刚刷新过，那一款可能已经不在了");
        assert!(board.versions.is_empty());
        assert!(board.opened().is_none());
    }
}

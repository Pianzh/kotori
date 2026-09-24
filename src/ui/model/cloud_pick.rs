//! 「自己选…」那个浮层：从**云端索引**里挑一条身份绑上。
//!
//! 从 `model/add.rs` 拆出来（那边是本页那块匹配的状态机，这一块是它旁边那个选择器）。
//! 用途唯一：指纹没命中、命中多条、或者用户就是知道云端那一条叫什么 —— 让他自己挑。
//!
//! 照「挑一个进程」那套做（`model::picker`）：候选是**打开时取的那一份快照**，过滤在
//! 内存里做。每敲一个字都去读一次云端索引太吵，而桶里那些游戏本来就不在这几秒里变。

use super::cloud::{self, CloudGameRow, CloudListReply};

/// 打开这个浮层是为了哪一件事（两处共用它，挑中之后干的不一样）。
///
/// ⚠ `pub`（不是 `pub(in crate::ui)`）：它出现在 `Message::CloudPickOpen` 这个公开枚举里
/// —— 与 `CloudListReply` 当初同一个理由（私有类型不许出现在公开接口上）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloudPickPurpose {
    /// 添加游戏页：挑中的那条成为这一款的云端身份（`add_match.pick`）。
    #[default]
    Add,
    /// 启动前那一问：挑中的那条就是"这一款在云端是谁"，挑完接着启动（`sync.resolve`）。
    Launch,
    /// 单游戏页的「更改绑定…」：挑中的那条就是新的绑定，**不启动游戏**。这一种用途下
    /// 浮层底部会多一条出路：新建一条身份（用户 2026-09-24 要的，且要确认才生效）。
    Rebind,
}

impl CloudPickPurpose {
    /// 这个用途下浮层里要不要给"新建一条云端身份"那颗按钮。
    pub(in crate::ui) fn can_create_identity(self) -> bool {
        matches!(self, CloudPickPurpose::Rebind)
    }
}

/// 浮层的状态。
#[derive(Debug, Default)]
pub(in crate::ui) struct CloudPick {
    purpose: CloudPickPurpose,
    open: bool,
    loading: bool,
    /// 桶里建过索引没有。`false` 时候选必然是空的 —— 那句话要说清"去深扫一次"。
    indexed: bool,
    /// 这份清单是从本机缓存来的、什么时候拿到的、上次刷新成不成（给用户看的那句话）。
    from_cache: bool,
    cached_at: String,
    refresh_error: Option<String>,
    /// 打开时取的全量候选。
    all: Vec<CloudGameRow>,
    /// 过滤后的那一份（推给窗口的就是它）。
    filtered: Vec<CloudGameRow>,
    query: String,
    error: Option<String>,
}

impl CloudPick {
    /// 打开浮层并挂上"取候选"的那一次请求（调用方负责发它）。
    pub(in crate::ui) fn open(&mut self, purpose: CloudPickPurpose) {
        self.purpose = purpose;
        self.open = true;
        self.loading = true;
        self.error = None;
        self.all.clear();
        self.filtered.clear();
        self.query.clear();
    }

    pub(in crate::ui) fn close(&mut self) {
        self.open = false;
        self.loading = false;
    }

    /// 候选到了。搜索词此刻可能已经打了一半，所以照它重算一遍。
    ///
    /// "这份清单从哪儿来、什么时候拿到的"也一起收下 —— 用户 2026-09-23 要在界面上看到
    /// 那个时间（索引平时读的是本机缓存，可能是旧的）。
    pub(in crate::ui) fn loaded(&mut self, reply: CloudListReply) {
        self.indexed = reply.indexed;
        self.from_cache = reply.from_cache;
        self.cached_at = reply.cached_at;
        self.refresh_error = reply.refresh_error;
        self.all = reply.rows;
        self.loading = false;
        self.error = None;
        self.refilter();
    }

    /// 取候选失败：说清楚，别让浮层空着像"云端没有游戏"。
    pub(in crate::ui) fn failed(&mut self, error: String) {
        self.all.clear();
        self.filtered.clear();
        self.loading = false;
        self.error = Some(error);
    }

    pub(in crate::ui) fn set_query(&mut self, query: String) {
        self.query = query;
        self.refilter();
    }

    fn refilter(&mut self) {
        let query = self.query.clone();
        self.filtered = self
            .all
            .iter()
            .filter(|row| row.matches(&query))
            .cloned()
            .collect();
    }

    pub(in crate::ui) fn is_open(&self) -> bool {
        self.open
    }

    /// 这一次打开是为了什么（`CloudPickChoose` 按它分派）。
    pub(in crate::ui) fn purpose(&self) -> CloudPickPurpose {
        self.purpose
    }

    pub(in crate::ui) fn loading(&self) -> bool {
        self.loading
    }

    pub(in crate::ui) fn query(&self) -> &str {
        &self.query
    }

    /// 推给窗口的那一份（已经过滤过）。
    pub(in crate::ui) fn rows(&self) -> &[CloudGameRow] {
        &self.filtered
    }

    /// 用户点了某一行：把那条**从全量里**取出来并收起浮层。
    ///
    /// 按 `cloud_id` 找而不是按行号：行号是过滤后那一份的，而 `cloud_id` 在索引里唯一
    /// （合并就是按它），所以搜索词怎么变都不会认错人。
    pub(in crate::ui) fn pick(&mut self, cloud_id: &str) -> Option<CloudGameRow> {
        let row = self
            .all
            .iter()
            .find(|row| row.cloud_id == cloud_id)
            .cloned();
        if row.is_some() {
            self.close();
        }
        row
    }

    /// 浮层里那行小字：出错优先，其次"读取中"、没索引、云端是空的，最后才是"滤掉了多少"。
    ///
    /// 不管哪一种情况，后面都缀上"这份清单是什么时候拿到的" —— 索引读的是本机缓存，
    /// 用户得知道它可能旧了一小时（用户 2026-09-23 要显示时间）。
    pub(in crate::ui) fn message(&self) -> String {
        let line = if let Some(error) = &self.error {
            format!("读云端清单失败: {error}\n这一款照旧可以添加 —— 只是没法绑到云端那一条上。")
        } else if self.loading {
            "正在读云端清单…".to_string()
        } else if !self.indexed {
            "桶里还没有这份索引：到「云端存档」页点一次「深度扫描云端」，之后再回来挑。".to_string()
        } else if self.all.is_empty() {
            "云端还没有游戏。这一款添加之后第一次上传会新建一条身份。".to_string()
        } else {
            let hidden = self.all.len() - self.filtered.len();
            if !self.query.trim().is_empty() && hidden > 0 {
                format!("云端 {} 款，其中 {hidden} 款被搜索词滤掉了", self.all.len())
            } else {
                String::new()
            }
        };

        let mut lines: Vec<String> = Vec::new();
        if !line.is_empty() {
            lines.push(line);
        }
        let source = cloud::source_label(self.from_cache, &self.cached_at);
        if !source.is_empty() {
            lines.push(source);
        }
        if let Some(trouble) = cloud::trouble_label(self.refresh_error.as_deref()) {
            lines.push(trouble);
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份"刚从云端读出来"的回包（这些测试关心的是候选，不是来源）。
    fn reply(indexed: bool, rows: Vec<CloudGameRow>) -> CloudListReply {
        CloudListReply {
            indexed,
            from_cache: false,
            cached_at: "20260923T101500Z".to_string(),
            refresh_error: None,
            rows,
        }
    }

    fn row(cloud_id: &str, name: &str) -> CloudGameRow {
        CloudGameRow {
            cloud_key: format!("key-{cloud_id}"),
            cloud_id: cloud_id.to_string(),
            name: name.to_string(),
            machines: 1,
            versions: 3,
            latest: Some("20260911T101500Z".to_string()),
            size: 4096,
            ..CloudGameRow::default()
        }
    }

    /// 搜索词命中名字、落点、用过的 exe 路径（`CloudGameRow::matches`），并说清滤掉了多少。
    #[test]
    fn the_query_filters_the_snapshot_and_says_what_it_hid() {
        let mut pick = CloudPick::default();
        pick.open(CloudPickPurpose::Add);
        assert!(pick.loading(), "刚打开时是在读");
        assert!(pick.rows().is_empty());

        pick.loaded(reply(
            true,
            vec![
                row("c1", "云端记下的名字"),
                CloudGameRow {
                    exe_paths: vec![r"D:\Games\Hoshi\game.exe".to_string()],
                    ..row("c2", "另一款")
                },
            ],
        ));
        assert!(!pick.loading());
        assert_eq!(pick.rows().len(), 2);

        pick.set_query("hoshi".into());
        assert_eq!(pick.rows().len(), 1, "用过的 exe 路径也是搜索参数");
        assert_eq!(pick.rows()[0].cloud_id, "c2");
        pick.set_query("zzz".into());
        assert!(pick.rows().is_empty());
        assert!(pick.message().contains("滤掉"), "{}", pick.message());

        pick.set_query(String::new());
        // 没被滤掉时不该有"滤掉了多少"那句；留下来的只有"这份清单什么时候拿到的"
        //（用户 2026-09-23 要显示时间，所以它一直在）。
        assert!(!pick.message().contains("滤掉"), "{}", pick.message());
        assert!(
            pick.message().starts_with("刚从云端读的 · "),
            "{}",
            pick.message()
        );
    }

    /// 挑中的必须是**全量**里那一条，而且挑完就收起浮层 —— 行号会随搜索词变，`cloud_id`
    /// 不会（索引里它唯一）。
    #[test]
    fn picking_goes_by_cloud_id_and_closes_the_sheet() {
        let mut pick = CloudPick::default();
        pick.open(CloudPickPurpose::Add);
        pick.loaded(reply(true, vec![row("c1", "一号"), row("c2", "二号")]));
        pick.set_query("二号".into());
        assert_eq!(pick.rows().len(), 1);

        let picked = pick.pick("c2").expect("这条在候选里");
        assert_eq!(picked.name, "二号");
        assert!(!pick.is_open(), "挑完就收起来");
        assert!(pick.pick("c9").is_none(), "不在候选里的 id 挑不动");
    }

    /// 重开一次：上一次的候选与搜索词都不许留着。
    #[test]
    fn opening_again_starts_from_a_clean_slate() {
        let mut pick = CloudPick::default();
        pick.open(CloudPickPurpose::Add);
        pick.loaded(reply(true, vec![row("c1", "一号")]));
        pick.set_query("一号".into());
        pick.close();

        pick.open(CloudPickPurpose::Add);
        assert!(pick.rows().is_empty());
        assert_eq!(pick.query(), "");
        assert!(pick.loading());
    }

    /// 四种"列表是空的"要说四句不同的话：还没建索引 / 云端确实没有 / 读不成 /（读取中）。
    #[test]
    fn an_empty_list_never_looks_like_an_empty_cloud() {
        let mut pick = CloudPick::default();
        pick.open(CloudPickPurpose::Add);
        pick.loaded(reply(false, Vec::new()));
        assert!(
            pick.message().contains("深度扫描云端"),
            "{}",
            pick.message()
        );

        pick.open(CloudPickPurpose::Add);
        pick.loaded(reply(true, Vec::new()));
        assert!(
            pick.message().contains("云端还没有游戏"),
            "{}",
            pick.message()
        );

        pick.open(CloudPickPurpose::Add);
        pick.failed("连不上桶".to_string());
        assert!(pick.message().contains("连不上桶"), "{}", pick.message());
        assert!(
            pick.message().contains("照旧可以添加"),
            "{}",
            pick.message()
        );
        assert!(pick.rows().is_empty());
    }
}

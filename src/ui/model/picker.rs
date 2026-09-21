//! 「从正在运行的进程里挑」:候选与搜索词。
//!
//! 它只服务添加游戏页那一条路(挑一个正在跑的进程 → 填成一条新档案),所以状态放在
//! `App` 上、浮层挂在窗口根上(`app.slint`),而不是页面自己画一份。
//!
//! 候选是**打开时取的那一份快照**:过滤在内存里做。每敲一个字都去问一次 daemon 太吵,
//! 而进程表本来就是"当时那一瞬"的东西。

/// 浮层里的一行(与 `.slint` 的 `ProcessPickRow` 一一对应)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: i32,
    /// 进程名(平台报的那一份,可能被截断)。
    pub name: String,
    /// 窗口标题;空 = 拿不到(Windows 有,Linux 通常没有)。
    pub title: String,
    /// exe 完整路径;空 = 拿不到。
    pub exe: String,
}

impl ProcessRow {
    /// 搜索词命中吗?进程名、窗口标题、PID、路径都算 —— 用户手上有什么就搜什么。
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(&query)
            || self.title.to_lowercase().contains(&query)
            || self.exe.to_lowercase().contains(&query)
            || self.pid.to_string().contains(&query)
    }
}

/// 浮层顶上那句话(它只有一个用途:挑一个正在跑的进程,把它填成一条新档案)。
pub const PICKER_TITLE: &str = "挑一个进程添加成游戏";

/// 浮层的状态。
#[derive(Debug, Default)]
pub(in crate::ui) struct ProcessPicker {
    /// 打开时取一次的全量候选。
    all: Vec<ProcessRow>,
    /// 过滤后的那一份(推给窗口的就是它)。
    filtered: Vec<ProcessRow>,
    query: String,
    open: bool,
    loading: bool,
    error: Option<String>,
}

impl ProcessPicker {
    /// 打开浮层并挂上"取候选"的那一次请求(调用方负责发它)。
    pub(in crate::ui) fn open(&mut self) {
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

    /// 候选到了。过滤条件(搜索词)此刻可能已经打了一半,所以照它重算一遍。
    pub(in crate::ui) fn loaded(&mut self, rows: Vec<ProcessRow>) {
        self.all = rows;
        self.loading = false;
        self.error = None;
        self.refilter();
    }

    /// 取候选失败:说清楚,别让浮层空着像"没有进程"。
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

    pub(in crate::ui) fn loading(&self) -> bool {
        self.loading
    }

    pub(in crate::ui) fn query(&self) -> &str {
        &self.query
    }

    pub(in crate::ui) fn title(&self) -> &'static str {
        PICKER_TITLE
    }

    /// 推给窗口的那一份(已经过滤过)。
    pub(in crate::ui) fn rows(&self) -> &[ProcessRow] {
        &self.filtered
    }

    /// 第 `index` 行(下标是**过滤后**那一份里的 —— 界面点的就是它的下标)。
    pub(in crate::ui) fn row(&self, index: usize) -> Option<&ProcessRow> {
        self.filtered.get(index)
    }

    /// 浮层里那行小字:出错优先,其次"读取中",其余留空(列表自己会说话)。
    pub(in crate::ui) fn message(&self) -> String {
        if let Some(error) = &self.error {
            return format!("读取进程列表失败: {error}");
        }
        if self.loading {
            return "正在读取进程列表…".to_string();
        }
        let hidden = self.all.len() - self.filtered.len();
        if !self.query.trim().is_empty() && hidden > 0 {
            return format!("{} 个候选,其中 {hidden} 个被搜索词滤掉了", self.all.len());
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ProcessRow> {
        vec![
            ProcessRow {
                pid: 4321,
                name: "Game.exe".into(),
                title: "BLACKSOULS Ⅱ".into(),
                exe: r"Z:\run\media\disk\BLACKSOULS\Game.exe".into(),
            },
            ProcessRow {
                pid: 99,
                name: "GAEL.exe".into(),
                title: String::new(),
                exe: String::new(),
            },
        ]
    }

    /// 搜索词对**进程名、窗口标题、PID、路径**都要命中 —— 用户手上有什么就搜什么;
    /// 而滤掉多少要让用户看得见(否则"我明明开着游戏"会被当成 bug)。
    #[test]
    fn the_query_matches_every_clue_and_says_what_it_hid() {
        let mut picker = ProcessPicker::default();
        picker.open();
        picker.loaded(rows());
        assert_eq!(picker.rows().len(), 2);

        picker.set_query("black".into());
        assert_eq!(picker.rows().len(), 1, "标题命中");
        picker.set_query("gael".into());
        assert_eq!(picker.rows().len(), 1, "进程名命中");
        picker.set_query("99".into());
        assert_eq!(picker.rows().len(), 1, "PID 命中");
        picker.set_query("media".into());
        assert_eq!(picker.rows().len(), 1, "路径命中");

        picker.set_query("zzz".into());
        assert!(picker.rows().is_empty());
        assert!(picker.message().contains("滤掉"), "{}", picker.message());

        picker.set_query(String::new());
        assert_eq!(picker.rows().len(), 2);
        assert_eq!(picker.message(), "", "没被滤掉就别说话");
    }

    /// 挑中的必须是**过滤后**那一份里的下标 —— 界面上点的就是它的行号。
    #[test]
    fn picking_uses_the_filtered_index() {
        let mut picker = ProcessPicker::default();
        picker.open();
        picker.loaded(rows());
        picker.set_query("gael".into());
        assert_eq!(picker.row(0).unwrap().pid, 99);
        assert_eq!(picker.row(1), None);
    }

    /// 重开一次:上一次的候选与搜索词都不许留着(否则新开时会短暂显示旧列表)。
    #[test]
    fn opening_again_starts_from_a_clean_slate() {
        let mut picker = ProcessPicker::default();
        picker.open();
        picker.loaded(rows());
        picker.set_query("black".into());
        picker.close();

        picker.open();
        assert!(picker.rows().is_empty());
        assert_eq!(picker.query(), "");
        assert!(picker.loading(), "刚打开时是在读");
        assert_eq!(picker.title(), "挑一个进程添加成游戏");
    }
}

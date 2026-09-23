//! 「云端存档」那一块的状态：云端有哪几款、每款几版、点开的那一款有哪几版。
//!
//! 与 `sync`（本机的设置与凭据）分开：这一块**只读云端**，一个字都不改本机配置，
//! 而且它记的是"上次按刷新时看到的样子"——云端会在别的机器上变，所以它天生是
//! 一张快照，不是实时状态。措辞在这里定，`.slint` 只管画（与配对表同一条规矩）。

/// 云端的一款游戏（`sync.cloud_games` 的一行）。
///
/// ⚠ `key` 是**云端落点**（rclone 是目录名，kopia 是 `game:` 标签值），不是本机
/// id：云端有而本机没有的游戏也要能列出来，它没有 id 可用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudSaveRow {
    pub key: String,
    /// 云端这一款有几版。
    pub versions: usize,
}

impl CloudSaveRow {
    /// 这一行右半边的那句话。
    pub(in crate::ui) fn versions_label(&self) -> String {
        match self.versions {
            0 => "还没有存档".to_string(),
            count => format!("{count} 版"),
        }
    }
}

/// 「云端存档」这一块的全部状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBoard {
    /// 云端有哪些游戏。空数组 + `scanned` = 云端还没有游戏。
    pub rows: Vec<CloudSaveRow>,
    pub scanned: bool,
    pub loading: bool,
    /// 一句话状态（列完 / 出错）。空 = 什么都别说。
    pub msg: Option<String>,
    pub ok: bool,
    /// 点开的那一款（`None` = 都收着）。
    pub open: Option<String>,
    /// 点开那一款的版本名，最旧在前（与云端列出来的顺序一致）。
    pub versions: Vec<String>,
    pub versions_loading: bool,
}

impl Default for CloudBoard {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            scanned: false,
            loading: false,
            msg: None,
            // 还没出过错 —— 第一句话不该是红色。
            ok: true,
            open: None,
            versions: Vec::new(),
            versions_loading: false,
        }
    }
}

impl CloudBoard {
    /// 点一下某一款：开着的那一款收起来，别的换成它。
    ///
    /// 返回 `true` 表示**要去云端问一次版本**——收起来不用问，这是这个函数唯一的
    /// 判断，所以它落在这里被单测盯住。
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

    /// 某一款的版本回来了。
    ///
    /// ⚠ **只认还开着的那一款**：两份回包可能乱序（点开甲、又点开乙，甲的回包后到），
    /// 把甲的结果铺到乙底下就是给用户看错东西。
    pub(in crate::ui) fn versions_loaded(&mut self, key: &str, versions: Vec<String>) {
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

    /// 云端回来了：整张表换掉，顺手把点开的那一款收起来（它可能已经不在了）。
    pub(in crate::ui) fn loaded(&mut self, rows: Vec<CloudSaveRow>) {
        self.rows = rows;
        self.scanned = true;
        self.loading = false;
        self.open = None;
        self.versions.clear();
        self.versions_loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> CloudBoard {
        CloudBoard {
            rows: vec![
                CloudSaveRow {
                    key: "demo".into(),
                    versions: 3,
                },
                CloudSaveRow {
                    key: "other".into(),
                    versions: 0,
                },
            ],
            scanned: true,
            ..CloudBoard::default()
        }
    }

    #[test]
    fn a_new_board_is_quiet_and_not_red() {
        let fresh = CloudBoard::default();
        assert!(fresh.msg.is_none());
        assert!(fresh.ok, "还没出过错，第一句话不该是红的");
        assert!(!fresh.scanned && !fresh.loading);
        assert_eq!(fresh.rows.len(), 0);
    }

    #[test]
    fn each_row_says_how_many_versions_it_has() {
        let board = board();
        assert_eq!(board.rows[0].versions_label(), "3 版");
        // 「0 版」是句机器话：零版就是"还没有存档"。
        assert_eq!(board.rows[1].versions_label(), "还没有存档");
    }

    #[test]
    fn clicking_a_row_opens_it_and_clicking_again_closes_it() {
        let mut board = board();
        assert!(board.toggle("demo"), "点开要去云端问版本");
        assert_eq!(board.open.as_deref(), Some("demo"));
        assert!(board.versions_loading);

        // 收起不用问云端 —— 这一条是这个函数的全部意义。
        assert!(!board.toggle("demo"));
        assert_eq!(board.open, None);
        assert!(!board.versions_loading);

        // 换一款：旧的版本立刻清掉，不能拿甲的历史垫在乙底下。
        board.toggle("demo");
        board.versions = vec!["20260911T101500Z".into()];
        board.versions_loading = false;
        assert!(board.toggle("other"));
        assert_eq!(board.open.as_deref(), Some("other"));
        assert!(board.versions.is_empty());
    }

    #[test]
    fn a_late_reply_never_lands_under_the_wrong_game() {
        let mut board = board();
        board.toggle("demo");
        board.toggle("other");
        // 甲的回包后到：必须丢掉，而不是铺到乙底下。
        board.versions_loaded("demo", vec!["20260911T101500Z".into()]);
        assert!(board.versions.is_empty(), "{:?}", board.versions);
        assert!(board.versions_loading, "还在等乙的版本");
        board.versions_loaded("other", vec!["20260910T090000Z".into()]);
        assert_eq!(board.versions, vec!["20260910T090000Z".to_string()]);
        assert!(!board.versions_loading);

        // 报错走同一条判据。
        board.versions_failed("demo", "列的途中断了".into());
        assert!(board.ok && board.msg.is_none(), "别人的错不该挂在这一款上");
        board.versions_failed("other", "列的途中断了".into());
        assert!(!board.ok);
        assert_eq!(board.msg.as_deref(), Some("列的途中断了"));
        assert!(board.versions.is_empty());
        assert!(!board.versions_loading);
    }

    #[test]
    fn refreshing_closes_whatever_was_open() {
        let mut board = board();
        board.toggle("gone");
        board.versions = vec!["20260911T101500Z".into()];
        board.loaded(vec![CloudSaveRow {
            key: "demo".into(),
            versions: 5,
        }]);
        assert!(board.scanned && !board.loading);
        assert_eq!(board.open, None, "刚刷新过，那一款可能已经不在了");
        assert!(board.versions.is_empty());
        assert_eq!(board.rows[0].versions_label(), "5 版");
    }
}

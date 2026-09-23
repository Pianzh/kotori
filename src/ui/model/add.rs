//! 「添加游戏」页那一块**云端匹配**：填完 exe 之后问一次"云端有没有这一款"。
//!
//! 用户 2026-09-23 定的流程：exe 是唯一必填项，写完就去云端认这一款；认出来就在**本页**
//! 直接确定（认领走的是已有的 `sync.pair`，见 `update/add.rs`）。判据只有指纹。
//!
//! ⚠ 这一块**永远不许挡住添加**：没配云同步、网络不通、指纹读不出，全都只是本页的一句话，
//! 添加照旧 —— 第一次上传时还会再认一次（那条路一直在）。
//!
//! 状态机只有四个相位，但有两个容易错的地方，都在下面的方法里钉住了：
//!   * **防抖**：敲路径时每来一个字符就问一次云端是荒唐的（读文件 + 读索引），所以先记
//!     `pending`，到点了才问（见 `typing` / `still_pending`）；
//!   * **迟到的回包**：改了 exe 之后，上一次的问话可能才回来 —— 只有"正在问的那一个"
//!     的回包才算数（见 `loaded` / `failed`），否则会把别的游戏的版本数铺到这一款头上。

use super::cloud::CloudGameRow;

/// 敲完之后等这么久才去问云端。
///
/// 与自动保存同一个理由（[`super::game::AUTOSAVE_DEBOUNCE`]）：打一串路径只问最后一次。
/// 比自动保存短一些 —— 用户点完「浏览…」之后就在等这句话。
pub(in crate::ui) const MATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(350);

/// 匹配走到哪一步了。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::ui) enum MatchPhase {
    /// 没什么可问的：exe 还空着、路径上没有这个文件，或者用户还在敲（等防抖）。
    #[default]
    Idle,
    /// 正在问云端（读一次索引）。
    Asking,
    /// 问到了 —— `rows` 为空表示云端确实没有这一款。
    Ready,
    /// 没问成（没配云同步 / 索引读不到 / 指纹算不出）。**不影响添加**。
    Failed(String),
}

/// 添加页上这块"云端匹配"的全部状态。
#[derive(Debug, Default)]
pub(in crate::ui) struct AddMatch {
    pub phase: MatchPhase,
    /// 云端对得上这个指纹的那些（`indexed == false` 时恒空）。
    pub rows: Vec<CloudGameRow>,
    /// 桶里建过索引没有：`false` 时"没有命中"其实是"还没建索引"，两句话不一样。
    pub indexed: bool,
    /// 用户挑中的那一条（按 `cloud_id`）；`None` = 还没挑（或多条还没选）。
    pub chosen: Option<String>,
    /// 用户点了「不是这一款」：添加照旧，只是不与云端绑定。
    pub declined: bool,
    /// 等防抖的那个 exe。
    pending: Option<String>,
    /// 正在问的那个 exe —— 回包拿它比对，迟到的丢掉。
    asked: Option<String>,
}

impl AddMatch {
    /// exe 又变了：上一次的结果全部作废，并记下"这一位还在敲，等敲完再说"。
    pub(in crate::ui) fn typing(&mut self, exe: &str) {
        let pending = exe.to_string();
        *self = Self {
            pending: Some(pending),
            ..Self::default()
        };
    }

    /// 什么都别问、什么都别说（exe 空 / 路径上不是文件 / 添加完了清场）。
    pub(in crate::ui) fn reset(&mut self) {
        *self = Self::default();
    }

    /// 防抖到点了：这个 exe 还是"要问的那一个"吗？（用户可能已经接着改了。）
    pub(in crate::ui) fn still_pending(&self, exe: &str) -> bool {
        self.pending.as_deref() == Some(exe)
    }

    /// 真的开始问了。
    pub(in crate::ui) fn asking(&mut self, exe: &str) {
        self.phase = MatchPhase::Asking;
        self.asked = Some(exe.to_string());
    }

    /// 回包到了。
    pub(in crate::ui) fn loaded(&mut self, exe: &str, indexed: bool, rows: Vec<CloudGameRow>) {
        if self.asked.as_deref() != Some(exe) {
            return;
        }
        self.indexed = indexed;
        // 唯一命中就直接替用户选上 —— 用户要的正是"匹配成功就直接在本页确定"。
        // 多条命中时**不猜**（那正是配对唯一不可逆的那种错），列出来让他挑。
        self.chosen = match rows.len() {
            1 => Some(rows[0].cloud_id.clone()),
            _ => None,
        };
        self.rows = rows;
        self.phase = MatchPhase::Ready;
    }

    /// 没问成。这里存的是一句话，不是错误状态 —— 页面上照旧能点「添加游戏」。
    pub(in crate::ui) fn failed(&mut self, exe: &str, message: String) {
        if self.asked.as_deref() != Some(exe) {
            return;
        }
        self.rows.clear();
        self.chosen = None;
        self.phase = MatchPhase::Failed(message);
    }

    /// 挑一条。
    pub(in crate::ui) fn choose(&mut self, cloud_id: &str) {
        if self.rows.iter().any(|row| row.cloud_id == cloud_id) {
            self.chosen = Some(cloud_id.to_string());
            self.declined = false;
        }
    }

    /// 「不是这一款」。
    pub(in crate::ui) fn decline(&mut self) {
        self.declined = true;
        self.chosen = None;
    }

    /// 改主意（点错了、或者想绑上）：唯一命中时顺手替用户选回来。
    pub(in crate::ui) fn undo_decline(&mut self) {
        self.declined = false;
        if self.rows.len() == 1 {
            self.chosen = Some(self.rows[0].cloud_id.clone());
        }
    }

    /// 这一款添加之后要与云端哪一条绑定；`None` = 不绑（照新档走）。
    pub(in crate::ui) fn binding(&self) -> Option<&CloudGameRow> {
        if self.declined {
            return None;
        }
        let chosen = self.chosen.as_deref()?;
        self.rows.iter().find(|row| row.cloud_id == chosen)
    }

    /// 页面上要不要说点什么（exe 还没落定时什么都不说，免得一进页面就闪）。
    pub(in crate::ui) fn visible(&self) -> bool {
        self.phase != MatchPhase::Idle
    }

    pub(in crate::ui) fn busy(&self) -> bool {
        self.phase == MatchPhase::Asking
    }

    /// 这一块的标题（措辞在 Rust 这边算好，页面只管显示）。
    pub(in crate::ui) fn title(&self) -> String {
        match &self.phase {
            MatchPhase::Idle => String::new(),
            MatchPhase::Asking => "正在问云端有没有这一款…".to_string(),
            MatchPhase::Failed(_) => "没问成云端 —— 不影响添加".to_string(),
            MatchPhase::Ready if self.declined => "好，这一款不与云端绑定".to_string(),
            MatchPhase::Ready if !self.indexed => "云端还没建索引 —— 这一款先按新的加".to_string(),
            MatchPhase::Ready if self.rows.is_empty() => "云端没有这一款".to_string(),
            MatchPhase::Ready if self.rows.len() == 1 => {
                format!("云端已有这一款：《{}》", self.rows[0].name)
            }
            MatchPhase::Ready => format!("云端有 {} 条都对得上，挑一条", self.rows.len()),
        }
    }

    /// 标题下面那行小字（可以多行）。
    pub(in crate::ui) fn detail(&self) -> String {
        match &self.phase {
            MatchPhase::Idle | MatchPhase::Asking => String::new(),
            MatchPhase::Failed(message) => format!("{message}\n添加之后第一次上传时还会再认一次。"),
            MatchPhase::Ready if self.declined => {
                "添加之后第一次上传会在云端新建一条身份（以后想改可以到「云端存档」页配对）。"
                    .to_string()
            }
            MatchPhase::Ready if !self.indexed => {
                "桶里还没有这份索引：到「云端存档」页点一次「深度扫描云端」就能建。\
                 添加之后第一次上传时，它会自己认领云端那一条。"
                    .to_string()
            }
            MatchPhase::Ready if self.rows.is_empty() => {
                "添加之后第一次上传会在云端新建一条身份。".to_string()
            }
            MatchPhase::Ready if self.rows.len() == 1 => self.one_detail(&self.rows[0]),
            MatchPhase::Ready => "点一条绑上；不挑就按新的加（第一次上传时再认）。".to_string(),
        }
    }

    /// 唯一命中时的三行：几版 / 本机认过没有 / 绑上意味着什么。
    fn one_detail(&self, row: &CloudGameRow) -> String {
        let summary = match row.latest_label() {
            label if label.is_empty() => row.versions_label(),
            label => format!("{} · {}", row.versions_label(), label),
        };
        let mut lines = vec![summary];
        if !row.local_id.is_empty() {
            lines.push(format!(
                "本机《{}》已经认了它 —— 两条档案会共用同一条云端身份。",
                row.local_name
            ));
        }
        lines.push("添加后会与它绑定：两边以后的版本存在同一处。".to_string());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 敲一个字就问一次云端是荒唐的：防抖到点之前什么都没有。
    #[test]
    fn typing_waits_and_only_the_last_edit_asks() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        assert!(!m.visible(), "还在敲，页面上什么都不该出现");
        assert!(m.still_pending("/games/a/game.exe"));
        m.typing("/games/a/game2.exe");
        assert!(
            !m.still_pending("/games/a/game.exe"),
            "上一次的没敲完就被作废了"
        );
        assert!(m.still_pending("/games/a/game2.exe"));
    }

    /// 改动之后的回包（问的是上一个 exe）绝不许铺到新输入上。
    #[test]
    fn a_late_answer_is_thrown_away() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.typing("/games/b/other.exe");
        m.loaded("/games/a/game.exe", true, vec![row("c1", "旧的那一款")]);
        assert_eq!(m.phase, MatchPhase::Idle);
        assert!(m.rows.is_empty(), "迟到的回包被丢掉了");
        m.failed("/games/a/game.exe", "网络不通".to_string());
        assert_eq!(m.phase, MatchPhase::Idle, "失败的回包也一样");
    }

    /// 唯一命中就直接替用户选上（"匹配成功就直接在本页确定"）。
    #[test]
    fn a_single_hit_is_chosen_for_the_user() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.loaded("/games/a/game.exe", true, vec![row("c1", "那一款")]);
        assert_eq!(m.phase, MatchPhase::Ready);
        assert_eq!(m.binding().map(|r| r.cloud_id.as_str()), Some("c1"));
        assert!(m.title().contains("那一款"));
        assert!(m.detail().contains("3 版"), "{}", m.detail());
    }

    /// 多条命中**不猜**：列出来让用户挑（配对错了不可逆）。
    #[test]
    fn several_hits_are_never_guessed() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.loaded(
            "/games/a/game.exe",
            true,
            vec![row("c1", "一号"), row("c2", "二号")],
        );
        assert!(m.binding().is_none(), "还没挑就绝不绑");
        assert!(m.title().contains("2 条"));
        m.choose("c2");
        assert_eq!(m.binding().map(|r| r.cloud_id.as_str()), Some("c2"));
        // 不在候选里的 id 挑不动（回包被改坏时也不至于绑错）。
        m.choose("c9");
        assert_eq!(m.binding().map(|r| r.cloud_id.as_str()), Some("c2"));
    }

    /// 「不是这一款」＝不绑；改主意要能回来。
    #[test]
    fn declining_means_no_binding_until_the_user_changes_their_mind() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.loaded("/games/a/game.exe", true, vec![row("c1", "那一款")]);
        m.decline();
        assert!(m.binding().is_none(), "用户说了不是它");
        assert!(m.title().contains("不与云端绑定"), "{}", m.title());
        m.undo_decline();
        assert_eq!(
            m.binding().map(|r| r.cloud_id.as_str()),
            Some("c1"),
            "唯一命中时改主意应当把选择还回来"
        );
    }

    /// 三句不同的话：还没建索引 / 云端确实没有 / 问不成。都不挡添加。
    #[test]
    fn a_missing_index_and_an_empty_cloud_say_different_things() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.loaded("/games/a/game.exe", false, Vec::new());
        assert!(m.title().contains("还没建索引"), "{}", m.title());
        assert!(m.detail().contains("深度扫描云端"), "{}", m.detail());

        m.asking("/games/a/game.exe");
        m.loaded("/games/a/game.exe", true, Vec::new());
        assert!(m.title().contains("云端没有这一款"), "{}", m.title());

        m.asking("/games/a/game.exe");
        m.failed("/games/a/game.exe", "连不上桶".to_string());
        assert!(m.title().contains("不影响添加"), "{}", m.title());
        assert!(m.detail().contains("连不上桶"), "{}", m.detail());
        assert!(m.binding().is_none(), "问不成时绝不绑");
    }

    /// exe 变空 / 不是文件时，连结果带选择一起收掉。
    #[test]
    fn resetting_clears_everything() {
        let mut m = AddMatch::default();
        m.typing("/games/a/game.exe");
        m.asking("/games/a/game.exe");
        m.loaded("/games/a/game.exe", true, vec![row("c1", "那一款")]);
        m.reset();
        assert!(!m.visible());
        assert!(m.rows.is_empty());
        assert!(m.binding().is_none());
    }
}

//! 「云端存档」页再下一层：**一个存档**的管理页。
//!
//! 用户 2026-09-26 定的形态：云端存档页里点开某一款 → 看它每一版（条目化）→ 每一版再点进去
//! 就是这一页。这一页以后集成"存档管理"（下载 / 备注 / 保留策略之类），**现在只有删除**。
//!
//! ⚠ 它只碰云端：不掺任何本机交互（覆盖本机、上传那些是单游戏设置那页的事，见
//! `model::versions` 的文件头）。所以这里连"本机有没有这一款"都不知道，也不需要知道。

use super::cloud::CloudVersionRow;
use super::confirm::Confirmation;

/// `CloudVersionBoard` 的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::ui) struct CloudVersionState {
    /// 这一页开着没有（界面上那个 `open` 由 render 推）。
    pub open: bool,
    /// 云端落点 —— 删除按它认人（不是本机 id）。
    pub key: String,
    /// 这一款叫什么（标题用）。
    pub game_name: String,
    /// 这一版在桶里的原名字（给人看的时间与大小都在它身上）。
    pub version: String,
    pub label: String,
    pub size_label: String,
    /// 一句话状态（删成没成 / 没删成的原因）。
    pub msg: Option<String>,
    pub ok: bool,
    /// 正等着二次确认。
    pending: bool,
    /// 删除在路上（挡住连点第二下）。
    pub busy: bool,
}

impl CloudVersionState {
    /// 点开某一版：开页，并清掉上一次的残留。
    pub(in crate::ui) fn opened(&mut self, game_name: &str, key: &str, version: &CloudVersionRow) {
        self.open = true;
        self.key = key.to_string();
        self.game_name = game_name.to_string();
        self.version = version.name.clone();
        self.label = version.label();
        self.size_label = version.size_label();
        self.msg = None;
        self.ok = true;
        self.pending = false;
        self.busy = false;
    }

    /// 「← 返回」（以及"那一版已经没了"之后的收尾）：连弹窗一起收掉。
    pub(in crate::ui) fn closed(&mut self) {
        self.open = false;
        self.pending = false;
        self.busy = false;
        self.msg = None;
    }

    /// 点了「删除这一版」：只记下"要问了"，真正的动作等确认。
    pub(in crate::ui) fn requested(&mut self) {
        self.pending = true;
        self.msg = None;
        self.ok = true;
    }

    pub(in crate::ui) fn cancelled(&mut self) {
        self.pending = false;
    }

    /// 确认了：把"要删哪一版"交出去，界面进入忙。
    ///
    /// 返回 `(云端落点, 版本名)`；没有待确认的就是 `None`。
    pub(in crate::ui) fn confirmed(&mut self) -> Option<(String, String)> {
        if !self.pending {
            return None;
        }
        self.pending = false;
        self.busy = true;
        self.ok = true;
        self.msg = Some("正在删除…".to_string());
        Some((self.key.clone(), self.version.clone()))
    }

    /// 删完了。成了就把这一页**收掉**（那一版已经没了，留在这一页只看见一个空壳），并把
    /// "删掉了哪一版 + 那句话"交回给调用方去写在外层那一页上；没成就留在这一页，把原因
    /// 写在这里。
    pub(in crate::ui) fn deleted(
        &mut self,
        result: Result<String, String>,
    ) -> Option<(String, String)> {
        self.busy = false;
        match result {
            Ok(summary) => {
                let gone = (self.version.clone(), summary);
                self.closed();
                Some(gone)
            }
            Err(error) => {
                self.ok = false;
                self.msg = Some(format!("删除失败: {error}"));
                None
            }
        }
    }

    /// 弹窗要问的那件事。这一页只有一种动作，所以措辞是 [`Confirmation`] 里现成的那一条
    /// —— 与单游戏设置那页点「删除」时说的话一模一样（同一件事不说两种话）。
    pub(in crate::ui) fn confirmation(&self) -> Confirmation {
        Confirmation::DeleteVersion {
            version: self.version.clone(),
        }
    }

    /// 弹窗现在要问的那件事（`None` = 不画弹窗）。
    pub(in crate::ui) fn pending(&self) -> Option<Confirmation> {
        self.pending.then(|| self.confirmation())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(name: &str, size: u64) -> CloudVersionRow {
        CloudVersionRow {
            name: name.to_string(),
            size,
            time: "2026-09-01 00:00:00".to_string(),
        }
    }

    #[test]
    fn opening_a_version_clears_whatever_the_last_one_left_behind() {
        let mut state = CloudVersionState::default();
        state.opened("甲", "key-a", &version("v1", 1024));
        state.requested();
        state.deleted(Err("没删成".into()));

        state.opened("乙", "key-b", &version("v2", 2048));
        assert!(state.open && state.ok);
        assert!(state.msg.is_none() && !state.busy && !state.pending().is_some());
        assert_eq!(state.game_name, "乙");
        assert_eq!(state.key, "key-b");
        assert_eq!(state.size_label, "2.0 KiB");
    }

    #[test]
    fn deleting_waits_for_the_second_click() {
        let mut state = CloudVersionState::default();
        state.opened("甲", "key-a", &version("v1", 1024));

        state.requested();
        assert!(
            state.pending().is_some() && !state.busy,
            "确认之前不该在路上"
        );
        assert_eq!(
            state.confirmation(),
            Confirmation::DeleteVersion {
                version: "v1".into()
            }
        );

        state.cancelled();
        assert!(!state.pending().is_some());
        assert_eq!(state.confirmed(), None, "取消了就再也确认不出东西");

        state.requested();
        assert_eq!(
            state.confirmed(),
            Some(("key-a".to_string(), "v1".to_string()))
        );
        assert!(state.busy && state.msg.as_deref() == Some("正在删除…"));
        assert!(!state.pending().is_some(), "弹窗当场收掉");
    }

    #[test]
    fn a_successful_delete_closes_the_page_and_hands_the_version_back() {
        let mut state = CloudVersionState::default();
        state.opened("甲", "key-a", &version("v1", 1024));
        state.requested();
        state.confirmed();

        let gone = state.deleted(Ok("已删掉云端那一版，这一款还剩 1 版".into()));
        assert_eq!(
            gone,
            Some(("v1".to_string(), "已删掉云端那一版，这一款还剩 1 版".into()))
        );
        assert!(!state.open, "那一版没了，这一页就该收掉");
        assert!(!state.busy);
    }

    #[test]
    fn a_failed_delete_keeps_the_page_open_and_says_why() {
        let mut state = CloudVersionState::default();
        state.opened("甲", "key-a", &version("v1", 1024));
        state.requested();
        state.confirmed();

        assert_eq!(state.deleted(Err("云端没有这一版: v1".into())), None);
        assert!(state.open, "没删成就留在这儿，用户才知道出了问题");
        assert!(!state.busy && !state.ok);
        assert!(
            state.msg.as_deref().unwrap().starts_with("删除失败: "),
            "{:?}",
            state.msg
        );
    }
}

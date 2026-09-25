//! 单游戏页那一页「这一款的云端存档」：当前身份在云端存了哪几版，以及"用哪一版替换
//! 本机"那一步的二次确认。
//!
//! 与 `cloud`（那一页看的是"云端都有什么"，按云端的落点组织、只读）分开：这一页是**从本机
//! 这一款的角度**看它自己那一条身份，而且能做的事是破坏性的（覆盖本机存档目录），所以它
//! 自带一个"等确认"的状态机 —— 与云同步页那颗「恢复」同一个形状。

use super::cloud::CloudVersionRow;

/// `GameVersionsBoard` 的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::ui) struct VersionsState {
    /// 页面开着没有（界面上那个 `open` 由 render 推）。
    pub open: bool,
    /// 看的是哪一款（本机 id）—— 列版本与替换都按它认人。
    pub game_id: String,
    /// 这一款要用/已用的云端落点。空 = 算不出来，请求都不必发。
    pub cloud_key: String,
    pub loading: bool,
    /// 云端这一款的每一版，**最新在前**。
    pub rows: Vec<CloudVersionRow>,
    /// 一句话状态（替换成没成 / 读不成的原因）。
    pub msg: Option<String>,
    pub ok: bool,
    /// 正等着二次确认的那一版（版本名）。`None` = 没有弹窗。
    pub pending: Option<String>,
    /// 替换在路上（挡住连点第二下，也让弹窗那颗按钮灰着）。
    pub busy: bool,
}

impl VersionsState {
    /// 点开身份那一条：开页，并清掉上一次的残留。
    ///
    /// 要不要发请求由调用方看 `cloud_key` 决定 —— 这一层只管状态。
    pub(in crate::ui) fn opened(&mut self, game_id: &str, cloud_key: &str) {
        self.open = true;
        self.game_id = game_id.to_string();
        self.cloud_key = cloud_key.to_string();
        self.loading = !cloud_key.is_empty();
        self.rows.clear();
        self.msg = None;
        self.ok = true;
        self.pending = None;
        self.busy = false;
    }

    /// 「← 返回」：连弹窗一起收掉（回不去的状态不该留着）。
    pub(in crate::ui) fn closed(&mut self) {
        self.open = false;
        self.pending = None;
        self.busy = false;
    }

    /// 版本回来了。**最新在前** —— 这一页最想取的通常就是最近那一版。
    pub(in crate::ui) fn loaded(&mut self, mut rows: Vec<CloudVersionRow>) {
        rows.reverse();
        self.rows = rows;
        self.loading = false;
    }

    /// 版本没列成。
    pub(in crate::ui) fn failed(&mut self, message: String) {
        self.rows.clear();
        self.loading = false;
        self.ok = false;
        self.msg = Some(message);
    }

    /// 点了某一行的「替换」：只记下是哪一版，真正的动作等确认。
    pub(in crate::ui) fn requested(&mut self, version: &str) {
        self.pending = Some(version.to_string());
        self.msg = None;
        self.ok = true;
    }

    pub(in crate::ui) fn cancelled(&mut self) {
        self.pending = None;
    }

    /// 确认了：把那一版交出去，界面进入忙。
    ///
    /// 返回要去替换的那一版；没有待确认的就是 `None`（调用方什么都不做）。
    pub(in crate::ui) fn confirmed(&mut self) -> Option<String> {
        let version = self.pending.take()?;
        self.busy = true;
        self.msg = Some("正在替换…".to_string());
        self.ok = true;
        Some(version)
    }

    /// 替换完了（成或不成）。
    ///
    /// ⚠ 结果**落在这里**，不是 `sync_form.msg`：那句话只画在云同步页上，从单游戏页按的
    /// 动作写在那边，用户根本看不见（既有毛病，这一页绕开它）。
    pub(in crate::ui) fn done(&mut self, result: Result<String, String>) {
        self.busy = false;
        match result {
            Ok(summary) => {
                self.ok = true;
                self.msg = Some(summary);
            }
            Err(error) => {
                self.ok = false;
                self.msg = Some(format!("替换失败: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(name: &str) -> CloudVersionRow {
        CloudVersionRow {
            name: name.to_string(),
            size: 1024,
            time: String::new(),
        }
    }

    #[test]
    fn opening_clears_whatever_the_last_one_left_behind() {
        let mut state = VersionsState::default();
        state.opened("demo", "demo-key");
        state.loaded(vec![version("old"), version("new")]);
        state.requested("old");
        state.done(Err("上一次失败了".into()));

        // 换一款再看：上一款的版本、弹窗、那句话都不该跟过来。
        state.opened("other", "other-key");
        assert!(state.open && state.loading);
        assert!(state.rows.is_empty() && state.msg.is_none() && state.ok);
        assert_eq!(state.pending, None);
        assert!(!state.busy);
        assert_eq!(state.game_id, "other");
    }

    #[test]
    fn the_newest_version_is_listed_first() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        // daemon 给的是"最旧在前"；这一页要反过来（最想取的通常是最新那版）。
        state.loaded(vec![
            version("20260901T000000Z"),
            version("20260911T101500Z"),
        ]);
        assert!(!state.loading);
        assert_eq!(state.rows[0].name, "20260911T101500Z");
        assert_eq!(state.rows[1].name, "20260901T000000Z");
    }

    #[test]
    fn a_replace_waits_for_the_second_click_and_can_be_called_off() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.loaded(vec![version("20260911T101500Z")]);

        // 只点「替换」不会动任何东西：只记下是哪一版。
        state.requested("20260911T101500Z");
        assert_eq!(state.pending.as_deref(), Some("20260911T101500Z"));
        assert!(!state.busy, "确认之前不该在路上");

        state.cancelled();
        assert_eq!(state.pending, None);
        assert_eq!(state.confirmed(), None, "取消了就再也确认不出东西");
    }

    #[test]
    fn confirming_hands_over_that_one_version_and_shows_the_outcome_here() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.requested("20260911T101500Z");

        let version = state.confirmed().expect("有待确认的那一版");
        assert_eq!(version, "20260911T101500Z");
        assert!(state.busy && state.ok, "确认之后进入忙");
        assert_eq!(state.msg.as_deref(), Some("正在替换…"));
        assert_eq!(state.pending, None, "弹窗当场收掉");

        // 结果落在这一页自己的那句话上（不是云同步页那句，见 `done` 的注释）。
        state.done(Ok("已用云端那一版覆盖本机存档".into()));
        assert!(!state.busy && state.ok);
        assert!(state.msg.as_deref().unwrap().contains("覆盖"));

        state.requested("20260901T000000Z");
        assert_eq!(state.confirmed().as_deref(), Some("20260901T000000Z"));
        state.done(Err("连不上桶".into()));
        assert!(!state.busy && !state.ok);
        assert!(state.msg.as_deref().unwrap().contains("连不上桶"));
    }

    #[test]
    fn leaving_the_page_takes_the_dialog_with_it() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.requested("20260911T101500Z");
        state.closed();
        assert!(!state.open);
        assert_eq!(state.pending, None, "退回上一页不该留着弹窗");
    }
}

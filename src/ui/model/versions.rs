//! 单游戏页那一页「这一款的云端存档」：当前身份在云端存了哪几版，以及"用哪一版替换
//! 本机 / 删掉哪一版 / 清空这一款 / 把这一款从云端抹掉"这几步的二次确认。
//!
//! 与 `cloud`（那一页看的是"云端都有什么"，按云端的落点组织、只读）分开：这一页是**从本机
//! 这一款的角度**看它自己那一条身份。按用户 2026-09-26 定的分工，**本机 ↔ 云端的交互都
//! 归这一页**（上传、取回、覆盖本机，以及"把这一款在云端的存档清掉"），而纯云上的管理归
//! `cloud`。所以四种破坏性动作都在这里，确认之前一个请求都不发。
//!
//! 弹窗上的那几行字在 [`Confirmation`] 里（三个入口共用一套说法），这一层只管"现在问的是
//! 哪一件事"。

use super::cloud::CloudVersionRow;
use super::confirm::Confirmation;

/// `GameVersionsBoard` 的状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::ui) struct VersionsState {
    /// 页面开着没有（界面上那个 `open` 由 render 推）。
    pub open: bool,
    /// 看的是哪一款（本机 id）—— 替换按它认人。
    pub game_id: String,
    /// 这一款要用/已用的云端落点。空 = 算不出来，请求都不必发。
    pub cloud_key: String,
    pub loading: bool,
    /// 云端这一款的每一版，**最新在前**。
    pub rows: Vec<CloudVersionRow>,
    /// 一句话状态（替换成没成 / 删成没成 / 读不成的原因）。
    pub msg: Option<String>,
    pub ok: bool,
    /// 正等着二次确认的那件事。`None` = 没有弹窗。
    pending: Option<Confirmation>,
    /// 已经在路上的那件事：结果回来时要说对是"替换失败"还是"删除失败"。
    inflight: Option<Confirmation>,
    /// 请求在路上（挡住连点第二下，也让弹窗那颗按钮灰着）。
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
        self.inflight = None;
        self.busy = false;
    }

    /// 「← 返回」：连弹窗一起收掉（回不去的状态不该留着）。
    pub(in crate::ui) fn closed(&mut self) {
        self.open = false;
        self.pending = None;
        self.inflight = None;
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

    /// 点了某一行的「替换」/「删除」，或者页尾那两颗"整款"按钮：只记下要问哪一件事，
    /// 真正的动作等确认。
    pub(in crate::ui) fn requested(&mut self, action: Confirmation) {
        self.pending = Some(action);
        self.msg = None;
        self.ok = true;
    }

    pub(in crate::ui) fn cancelled(&mut self) {
        self.pending = None;
    }

    /// 确认了：把这件事交出去，界面进入忙。
    ///
    /// 返回要去办的那件事；没有待确认的就是 `None`（调用方什么都不做）。
    pub(in crate::ui) fn confirmed(&mut self) -> Option<Confirmation> {
        let action = self.pending.take()?;
        self.busy = true;
        self.ok = true;
        self.msg = Some(format!("正在{}…", action.verb()));
        self.inflight = Some(action.clone());
        Some(action)
    }

    /// 替换完了（成或不成）。
    pub(in crate::ui) fn replaced(&mut self, result: Result<String, String>) {
        let verb = self
            .inflight
            .take()
            .as_ref()
            .map_or("替换", Confirmation::verb);
        self.finish(result, verb);
    }

    /// 删完了（成或不成）：**顺手收拾本地列表** —— 删一版就少一行，清空/抹掉就整张空掉。
    ///
    /// 不留着旧列表等下一次读：这一页就摆在用户眼前，删完还挂着那一版是最刺眼的一种错。
    /// 失败了就什么都别动（云端没删掉，列表也不该少一行）。
    pub(in crate::ui) fn deleted(&mut self, result: Result<String, String>) {
        let action = self.inflight.take();
        if result.is_ok() {
            match action.as_ref() {
                Some(Confirmation::DeleteVersion { version }) => {
                    self.rows.retain(|row| row.name != *version);
                }
                Some(Confirmation::ClearVersions | Confirmation::ForgetIdentity) => {
                    self.rows.clear()
                }
                _ => {}
            }
        }
        let verb = action.as_ref().map_or("删除", Confirmation::verb);
        self.finish(result, verb);
    }

    /// 结果落在这里那句话上。
    ///
    /// ⚠ 同一族的结果**都落在这一页自己那句话**，不是 `sync_form.msg`：那句话只画在云同步
    /// 页上，从单游戏页按的动作写在那边，用户根本看不见（既有毛病，这一页绕开它）。
    fn finish(&mut self, result: Result<String, String>, verb: &str) {
        self.busy = false;
        match result {
            Ok(summary) => {
                self.ok = true;
                self.msg = Some(summary);
            }
            Err(error) => {
                self.ok = false;
                self.msg = Some(format!("{verb}失败: {error}"));
            }
        }
    }

    /// 弹窗现在要问的那件事（`None` = 不画弹窗）。
    pub(in crate::ui) fn pending(&self) -> Option<&Confirmation> {
        self.pending.as_ref()
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

    fn delete(name: &str) -> Confirmation {
        Confirmation::DeleteVersion {
            version: name.to_string(),
        }
    }

    #[test]
    fn opening_clears_whatever_the_last_one_left_behind() {
        let mut state = VersionsState::default();
        state.opened("demo", "demo-key");
        state.loaded(vec![version("old"), version("new")]);
        state.requested(delete("old"));
        state.replaced(Err("上一次失败了".into()));

        // 换一款再看：上一款的版本、弹窗、那句话都不该跟过来。
        state.opened("other", "other-key");
        assert!(state.open && state.loading);
        assert!(state.rows.is_empty() && state.msg.is_none() && state.ok);
        assert!(state.pending().is_none());
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
        state.requested(Confirmation::Replace {
            version: "20260911T101500Z".into(),
        });
        assert!(state.pending().is_some());
        assert!(!state.busy, "确认之前不该在路上");

        state.cancelled();
        assert!(state.pending().is_none());
        assert_eq!(state.confirmed(), None, "取消了就再也确认不出东西");
    }

    #[test]
    fn confirming_hands_over_that_one_version_and_shows_the_outcome_here() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.requested(Confirmation::Replace {
            version: "20260911T101500Z".into(),
        });

        let action = state.confirmed().expect("有待确认的那一版");
        assert_eq!(
            action,
            Confirmation::Replace {
                version: "20260911T101500Z".into()
            }
        );
        assert!(state.busy && state.ok, "确认之后进入忙");
        assert_eq!(state.msg.as_deref(), Some("正在替换…"));
        assert!(state.pending().is_none(), "弹窗当场收掉");

        // 结果落在这一页自己的那句话上（不是云同步页那句，见 `finish` 的注释）。
        state.replaced(Ok("已用云端那一版覆盖本机存档".into()));
        assert!(!state.busy && state.ok);
        assert!(state.msg.as_deref().unwrap().contains("覆盖"));

        state.requested(Confirmation::Replace {
            version: "20260901T000000Z".into(),
        });
        assert_eq!(
            state.confirmed(),
            Some(Confirmation::Replace {
                version: "20260901T000000Z".into()
            })
        );
        state.replaced(Err("连不上桶".into()));
        assert!(!state.busy && !state.ok);
        assert!(state.msg.as_deref().unwrap().contains("连不上桶"));
    }

    #[test]
    fn leaving_the_page_takes_the_dialog_with_it() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.requested(delete("20260911T101500Z"));
        state.closed();
        assert!(!state.open);
        assert!(state.pending().is_none(), "退回上一页不该留着弹窗");
    }

    #[test]
    fn deleting_one_version_takes_exactly_that_row_out_of_the_list() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.loaded(vec![
            version("20260901T000000Z"),
            version("20260911T101500Z"),
        ]);

        state.requested(delete("20260901T000000Z"));
        assert_eq!(
            state.confirmed().as_ref().map(Confirmation::verb),
            Some("删除")
        );
        assert_eq!(state.msg.as_deref(), Some("正在删除…"));
        state.deleted(Ok("已删掉云端那一版，这一款还剩 1 版".into()));

        assert!(!state.busy && state.ok);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].name, "20260911T101500Z");
        assert!(state.msg.as_deref().unwrap().contains("还剩 1 版"));
    }

    #[test]
    fn clearing_or_forgetting_empties_the_list_but_a_failure_changes_nothing() {
        let mut state = VersionsState::default();
        state.opened("demo", "key");
        state.loaded(vec![version("a"), version("b")]);

        // 失败：云端那一版没删掉，列表也不该少一行。
        state.requested(delete("a"));
        state.confirmed();
        state.deleted(Err("云端没有这一版: a".into()));
        assert_eq!(state.rows.len(), 2, "没删成就不许动列表");
        assert!(!state.ok);
        assert!(
            state.msg.as_deref().unwrap().starts_with("删除失败: "),
            "{:?}",
            state.msg
        );

        // 清空这一款：整张列表空掉，但身份还在（`cloud_key` 一个字没动）。
        state.requested(Confirmation::ClearVersions);
        state.confirmed();
        state.deleted(Ok("已清空这一款的云端存档（删掉 2 版），身份留着".into()));
        assert!(state.rows.is_empty());
        assert!(state.ok);
        assert_eq!(state.cloud_key, "key");

        // 抹掉词条同理。
        state.loaded(vec![version("c")]);
        state.requested(Confirmation::ForgetIdentity);
        state.confirmed();
        state.deleted(Ok("已把这一款从云端抹掉（1 版存档连身份一起）".into()));
        assert!(state.rows.is_empty());
        assert!(state.msg.as_deref().unwrap().contains("抹掉"));
    }
}

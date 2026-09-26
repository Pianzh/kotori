//! 破坏性动作的二次确认：问句 / 明细 / 后果 / 确认那颗按钮的字，**收在一处**。
//!
//! 同一件事会在三个入口上出现（单游戏设置那页的云端存档、云端存档页的一款详情、云端存档
//! 页的一个存档详情）—— 说法必须一模一样，不然同一件事在两处叫两个名字，用户会以为是两
//! 件事。所以这些字既不进 `.slint`（那样测不到），也不在三个页面里各抄一份。
//!
//! 真正的动作（要发给 daemon 什么）由各页自己的状态记着；这个枚举只管"怎么说"。

/// 一次破坏性动作的措辞。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum Confirmation {
    /// 用云端某一版**覆盖本机** —— 四种里唯一动本机、不动云端的那一个。
    Replace { version: String },
    /// 删掉云端某一版。
    DeleteVersion { version: String },
    /// 清空这一款在云端的全部存档（身份留着）。
    ClearVersions,
    /// 把这一款从云端抹掉：身份连它的存档一起。
    ForgetIdentity,
}

impl Confirmation {
    /// 动作的名字：忙时那句话、失败那句前缀都用它。
    pub(in crate::ui) fn verb(&self) -> &'static str {
        match self {
            Self::Replace { .. } => "替换",
            Self::DeleteVersion { .. } => "删除",
            Self::ClearVersions => "清空",
            Self::ForgetIdentity => "抹掉",
        }
    }

    /// 弹窗第一行（问句）。
    pub(in crate::ui) fn title(&self) -> &'static str {
        match self {
            Self::Replace { .. } => "用这一版替换本机的存档？",
            Self::DeleteVersion { .. } => "删掉云端的这一版？",
            Self::ClearVersions => "清空这一款的云端存档？",
            Self::ForgetIdentity => "把这一款从云端抹掉？",
        }
    }

    /// 弹窗里那一行明细：具体哪一版，或者"这一款现在有几版"。
    ///
    /// `count` = 这一页手上那份列表里有几版；两个"整款"动作靠它把**要删掉多少**说清楚
    /// （列表还没读回来时是空串，那就不画这一行）。
    pub(in crate::ui) fn detail(&self, count: usize) -> String {
        match self {
            Self::Replace { version } | Self::DeleteVersion { version } => version.clone(),
            Self::ClearVersions | Self::ForgetIdentity if count == 0 => String::new(),
            _ => format!("云端这一条身份现在有 {count} 版存档。"),
        }
    }

    /// 后果那段话 —— 四种各说各的，共同点是**说清能不能撤销、动了谁**。
    pub(in crate::ui) fn body(&self) -> &'static str {
        match self {
            Self::Replace { .. } => {
                "本机现在的进度会被盖掉，这一步不能撤销。想留一手，就先点一次「立即同步」，把当前状态也传到云端。"
            }
            Self::DeleteVersion { .. } => {
                "云端这一版会被删掉，本机的存档不动。删掉之后云端就再也取不回它了。"
            }
            Self::ClearVersions => {
                "云端这条身份下的所有存档都会删掉，本机不动。身份留着 —— 以后同步还是传到同一条。"
            }
            Self::ForgetIdentity => {
                "云端这条身份连它的全部存档一起删掉，不留没人认领的数据。本机还绑着它 —— 下次同步会按原来的身份在云端新建一条。"
            }
        }
    }

    /// 确认那颗按钮的字（短，因为它只在一颗按钮上）。
    pub(in crate::ui) fn label(&self) -> &'static str {
        match self {
            Self::Replace { .. } => "用这一版覆盖",
            Self::DeleteVersion { .. } => "删掉这一版",
            Self::ClearVersions => "清空存档",
            Self::ForgetIdentity => "从云端抹掉",
        }
    }

    /// 确认那颗按钮要不要走警示色：三个删除动作要，替换不要（它是"覆盖"，不是"删"）。
    pub(in crate::ui) fn danger(&self) -> bool {
        !matches!(self, Self::Replace { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_says_what_it_does_and_to_whom() {
        // ① 替换：动本机、不动云端，所以那一段必须点明"本机会被盖掉"，且不警示。
        let replace = Confirmation::Replace {
            version: "20260911T101500Z".into(),
        };
        assert_eq!(replace.title(), "用这一版替换本机的存档？");
        assert_eq!(replace.detail(3), "20260911T101500Z");
        assert!(replace.body().contains("本机"));
        assert!(replace.body().contains("不能撤销"));
        assert!(!replace.danger());
        assert_eq!(replace.verb(), "替换");

        // ② 删一版：明细还是那一版，但要说清"本机的存档不动"，而且要警示。
        let delete = Confirmation::DeleteVersion {
            version: "20260901T000000Z".into(),
        };
        assert_eq!(delete.detail(3), "20260901T000000Z");
        assert!(delete.body().contains("本机的存档不动"));
        assert!(delete.danger());
        assert_eq!(delete.label(), "删掉这一版");

        // ③ 清空这一款：没有具体版本，明细改成"现在有几版"——用户才知道要删掉多少。
        assert_eq!(
            Confirmation::ClearVersions.detail(2),
            "云端这一条身份现在有 2 版存档。"
        );
        assert!(Confirmation::ClearVersions.body().contains("身份留着"));
        assert!(Confirmation::ClearVersions.danger());

        // ④ 抹掉词条：必须说清"本机还绑着它"，不然用户以为本机也解绑了。
        assert!(Confirmation::ForgetIdentity.body().contains("本机还绑着它"));
        assert_eq!(Confirmation::ForgetIdentity.label(), "从云端抹掉");
        assert_eq!(Confirmation::ForgetIdentity.verb(), "抹掉");
    }

    /// 列表还没读回来（0 版）时不留一行空的明细在那儿。
    #[test]
    fn the_count_line_disappears_when_there_is_nothing_to_count() {
        assert_eq!(Confirmation::ClearVersions.detail(0), "");
        assert_eq!(Confirmation::ForgetIdentity.detail(0), "");
        // 具体某一版的动作不受它影响。
        assert_eq!(
            Confirmation::DeleteVersion {
                version: "v".into()
            }
            .detail(0),
            "v"
        );
    }
}

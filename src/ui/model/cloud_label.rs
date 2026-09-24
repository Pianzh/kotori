//! 云端一条身份"给人看的那两行"：名字 + 摘要。
//!
//! ⚠ **云端身份的文案只有这一处**：单游戏页「当前绑定」、启动前那一问里那条候选、添加页
//! 浮层的每一行，全都走它 —— 用户 2026-09-24："弹窗显示的近似游戏信息使用的是和设置页面
//! 给出信息一样的函数就可以了，方便后期统一修改。" 以后索引里多出什么（几台机器见过、
//! 最后同步时间、那一版多大……）都往 [`identity_summary`] 里加一处就够。
//!
//! 与 `cloud.rs`（那一页的状态机）分开，就为了这一件事：文案能单独改、单独测，谁都不该为了
//! 显示一行字去读那页的状态。

use super::cloud::human_size;

/// 一条云端身份"给人看的那两行"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct CloudIdentityLabel {
    /// 主标题：云端记下的名字；没有就退回身份短号。
    pub name: String,
    /// 下面那行摘要（纯文本，一行）。
    pub summary: String,
}

/// 见 [`CloudIdentityLabel`]。
pub(in crate::ui) fn identity_label(
    cloud_id: &str,
    cloud_key: &str,
    name: &str,
    versions: u64,
    latest: &str,
    size: u64,
) -> CloudIdentityLabel {
    CloudIdentityLabel {
        name: if name.is_empty() {
            format!("云端身份 {}", short_id(cloud_id))
        } else {
            name.to_string()
        },
        summary: identity_summary(cloud_id, cloud_key, versions, latest, size),
    }
}

/// 摘要怎么拼：**唯一的汇总点**（[`identity_label`] 下面那行）。
pub(in crate::ui) fn identity_summary(
    cloud_id: &str,
    cloud_key: &str,
    versions: u64,
    latest: &str,
    size: u64,
) -> String {
    let mut parts: Vec<String> = vec![format!("身份 {}", short_id(cloud_id))];
    if !cloud_key.is_empty() {
        parts.push(format!("落点 {cloud_key}"));
    }
    if versions > 0 {
        parts.push(format!("云端 {versions} 版"));
        let at = crate::sync::describe_stamp(latest);
        if !at.is_empty() {
            match size {
                0 => parts.push(format!("最近一版 {at}")),
                bytes => parts.push(format!("最近一版 {at} · {}", human_size(bytes))),
            }
        }
    }
    parts.join(" · ")
}

/// 身份短号（前 8 位）：界面上一眼能对上，又不至于占满一行。
pub(in crate::ui) fn short_id(cloud_id: &str) -> String {
    cloud_id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名字没有就退回短号 —— 索引还没刷新到时也要画得出东西来。
    #[test]
    fn a_missing_name_falls_back_to_the_short_id() {
        let label = identity_label("8f2c1234-5678", "demo", "", 0, "", 0);
        assert_eq!(label.name, "云端身份 8f2c1234");
        assert_eq!(label.summary, "身份 8f2c1234 · 落点 demo");
    }

    /// 摘要里那几栏：身份、落点、几版、最近一版与大小；没有版数就不提"最近一版"。
    #[test]
    fn the_summary_lists_what_the_index_knows() {
        let label = identity_label("c1", "games/demo", "某游戏", 3, "20260911T101500Z", 4096);
        assert_eq!(label.name, "某游戏");
        assert!(
            label
                .summary
                .starts_with("身份 c1 · 落点 games/demo · 云端 3 版 · 最近一版 "),
            "{}",
            label.summary
        );
        assert!(label.summary.contains("4.0 KiB"), "{}", label.summary);

        let empty = identity_summary("c1", "", 0, "", 0);
        assert_eq!(empty, "身份 c1");
    }
}

//! 弱匹配：本机一条档案与云端一条身份"像不像"，以及判据的强弱顺序。
//!
//! 用户 2026-09-24："关于存档位置，我们暂时以父目录名称来规定，这个存档位置我建议
//! 单开一个文件，便于未来拓展更多的算法，更多的弱匹配方式。"
//!
//! * **强判据只有一条**：exe 指纹 —— 同一个可执行文件就是同一款，只有它能自己动手。
//! * 弱判据（名字相同、存档位置的父目录名有交集）**只用来列候选**，永不自动绑：
//!   猜错的代价是把别人的存档铺进本机这一款（用户 2026-09-21："宁可不动，也不猜"）。
//!
//! 判据只看**云端索引里的身份字段**（[`GameIdentity`] / `MachineIdentity`），既不读身份
//! 卡也不碰网络 —— 用户 2026-09-24："其他所有查询都只查本地索引，最大化减少网络请求次数"。
//!
//! 指纹的口径（用户 2026-09-24）："游戏 A 同时有指纹 abc 的情况，假设指纹 b 命中，那么
//! 直接判断命中；但是如果多个游戏共有同一个指纹，也就是指纹 a 同时命中游戏 ABC，这样才
//! 需要问。" ⇒ 同一条身份自己的多个指纹、它名下的多台机器都**不算**歧义；数歧义数的是
//! "有几条**身份**带着这个指纹"（见 [`fingerprint_owners`]）。

use std::collections::HashMap;

use crate::sync::cloud::GameIdentity;

/// 判据的强弱：`Ord` 的顺序就是"谁更硬"，靠前（小）的更硬。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Likeness {
    /// exe 指纹命中（命中这条身份的**任意一个**指纹就算）。
    Fingerprint,
    /// 名字一模一样。
    Name,
    /// 存档位置的父目录名有交集。
    ParentDir,
}

impl Likeness {
    /// 回包里的写法（界面按它选措辞，见 `ui/model/sync.rs` 的 `evidence_label`）。
    ///
    /// ⚠ "父目录名"这一档对外仍然写作 `location`：界面上那句话是"存档位置像"，
    /// 换名字会让界面文案与已有回包一起变，而这一档的**判据**才是新东西。
    pub fn as_str(self) -> &'static str {
        match self {
            Likeness::Fingerprint => "fingerprint",
            Likeness::Name => "name",
            Likeness::ParentDir => "location",
        }
    }
}

/// 本机这一侧参与判断的几栏。
#[derive(Debug, Clone, Copy)]
pub struct LocalSide<'a> {
    /// 本机这一款的名字。
    pub name: &'a str,
    /// 这一款的 exe 指纹（没有就是空 —— **绝不编一个**）。
    pub fingerprints: &'a [String],
    /// 它配的存档位置的父目录名（见 [`crate::sync::remote_paths::parent_dir`]）。
    pub parents: &'a [String],
}

/// 这条身份像不像本机这一条？像就是**最强的那条判据**。
pub fn likeness(local: &LocalSide<'_>, identity: &GameIdentity) -> Option<Likeness> {
    if local
        .fingerprints
        .iter()
        .any(|fingerprint| identity.has_fingerprint(fingerprint))
    {
        return Some(Likeness::Fingerprint);
    }
    if local.name == identity.name {
        return Some(Likeness::Name);
    }
    if shared_parents(local.parents, identity) > 0 {
        return Some(Likeness::ParentDir);
    }
    None
}

/// 父目录名重合了几个（同一档里用它排序：重合越多越像）。
pub fn shared_parents(local: &[String], identity: &GameIdentity) -> usize {
    local
        .iter()
        .filter(|name| {
            identity
                .machines
                .iter()
                .any(|machine| machine.parents.contains(name))
        })
        .count()
}

/// 云端每个指纹各被**几条身份**带着（同一条身份自己的多个指纹、名下的多台机器只算一次）。
///
/// 这是"要不要问用户"的唯一依据：`== 1` 才敢自动绑；`> 1` 就是"多个游戏共有同一个
/// 指纹"，必须问。
pub fn fingerprint_owners<'a>(
    identities: impl IntoIterator<Item = &'a GameIdentity>,
) -> HashMap<String, usize> {
    let mut owners: HashMap<String, usize> = HashMap::new();
    for identity in identities {
        let mut prints: Vec<&str> = identity
            .machines
            .iter()
            .flat_map(|machine| machine.fingerprints.iter().map(String::as_str))
            .collect();
        // 同一条身份里重复的指纹（多台机器各记了一遍）只算一次。
        prints.sort_unstable();
        prints.dedup();
        for print in prints {
            *owners.entry(print.to_string()).or_default() += 1;
        }
    }
    owners
}

/// 候选里"最像的那一条"。
///
/// ⚠ **现在的规则就是取第一条**。用户 2026-09-24："指纹命中多个游戏弹出最像的一个（什么
/// 是最像，现在还没有思路与确定，可以写个空函数默认取第一个或者随机取一个，这个问题就交给
/// 以后了）"。以后有更好的判据（名字相似度、厂商、VNDB 对齐……）**只改这一个函数** ——
/// 自检、弹窗、以后的批量对齐都不用动。
pub fn best_like<T>(candidates: &[T]) -> Option<&T> {
    candidates.first()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::cloud::MachineIdentity;

    fn machine(id: &str, prints: &[&str], parents: &[&str]) -> MachineIdentity {
        MachineIdentity {
            machine_id: id.to_string(),
            label: format!("host-{id}"),
            fingerprints: prints.iter().map(|print| print.to_string()).collect(),
            locations: Vec::new(),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            exe_paths: Vec::new(),
        }
    }

    fn identity(name: &str, machines: Vec<MachineIdentity>) -> GameIdentity {
        let mut identity = GameIdentity::new("cloud-1", name);
        identity.machines = machines;
        identity
    }

    fn side<'a>(name: &'a str, prints: &'a [String], parents: &'a [String]) -> LocalSide<'a> {
        LocalSide {
            name,
            fingerprints: prints,
            parents,
        }
    }

    /// 一条身份带着好几个指纹时，命中**任意一个**就算指纹命中（用户 2026-09-24 的口径）。
    #[test]
    fn any_of_the_fingerprints_counts_as_a_hit() {
        let cloud = identity("某游戏", vec![machine("a", &["p1", "p2", "p3"], &[])]);
        let prints = ["p2".to_string()];
        assert_eq!(
            likeness(&side("别的名字", &prints, &[]), &cloud),
            Some(Likeness::Fingerprint)
        );
    }

    /// 指纹 > 名字 > 父目录名：几样都像时给最硬的那条；一样都不像就是 `None`。
    #[test]
    fn the_strongest_likeness_wins() {
        let cloud = identity("某游戏", vec![machine("a", &["p1"], &["game"])]);
        let prints = ["p1".to_string()];
        let parents = ["game".to_string()];
        assert_eq!(
            likeness(&side("某游戏", &prints, &parents), &cloud),
            Some(Likeness::Fingerprint)
        );
        assert_eq!(
            likeness(&side("某游戏", &[], &parents), &cloud),
            Some(Likeness::Name)
        );
        assert_eq!(
            likeness(&side("别的", &[], &parents), &cloud),
            Some(Likeness::ParentDir)
        );
        let unrelated = ["other".to_string()];
        assert_eq!(likeness(&side("别的", &[], &unrelated), &cloud), None);
    }

    /// 数的是**身份**：同一条身份的多台机器、多个指纹都只算一条；两条身份带同一个指纹
    /// 才算 2（那才是"多个游戏共有同一个指纹"，要问）。
    #[test]
    fn owners_count_identities_not_machines() {
        let one = identity(
            "一号",
            vec![
                machine("a", &["dup"], &[]),
                machine("b", &["dup", "x"], &[]),
            ],
        );
        let two = identity("二号", vec![machine("c", &["dup"], &[])]);
        let owners = fingerprint_owners(&[one, two]);
        assert_eq!(owners.get("dup"), Some(&2), "两条身份带着它 ⇒ 要问");
        assert_eq!(owners.get("x"), Some(&1), "同一条身份里只算一次");
        assert_eq!(owners.get("nope"), None);
    }

    /// 父目录名按"有几个对得上"计数（同一档里排序用它）。
    #[test]
    fn shared_parents_counts_the_overlap() {
        let cloud = identity("某游戏", vec![machine("a", &[], &["game", "vendor"])]);
        assert_eq!(shared_parents(&["game".into(), "vendor".into()], &cloud), 2);
        assert_eq!(shared_parents(&["game".into(), "other".into()], &cloud), 1);
        assert_eq!(shared_parents(&["other".into()], &cloud), 0);
    }
}

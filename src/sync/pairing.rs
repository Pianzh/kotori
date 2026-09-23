//! 配对：把**云端的身份**与**本机的档案**对上号。
//!
//! 判据的强弱是有顺序的（用户 2026-09-21 定）：
//!   1. **exe 指纹** —— 同一个可执行文件，这就是同一款。唯一强到可以自己动手的判据；
//!   2. 名字相同、或者存档位置 key 有交集 —— 只能算"像"，**一律问用户**；
//!   3. 其余的一律不动。
//!
//! 自动绑定（第 1 条）只在"**恰好**一条本机档案命中、而且这条身份没被别人占用"时发生：
//! 命中两个（复制过存档目录、建了两条档案）就交给用户 —— 猜错的代价是把别人的存档
//! 铺进本机这一款，这条路上唯一不可逆的事。
//!
//! 本模块是**纯逻辑**：不碰配置、不碰网络。它拿"本机有哪些档案"和"云端有哪些身份"
//! 算出该绑什么、该问什么，daemon 负责把结果落盘、界面负责把结果画出来。
//!
//! ⚠ 身份卡里**没有 exe 的名字与大小**（§3.1 只放指纹）：所以"像"只能靠名字与存档
//! 位置，靠不上"exe 名 + 大小"那一档。

use crate::sync::cloud::GameIdentity;

/// 配对要看的那几栏本机档案信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGame {
    pub id: String,
    pub name: String,
    /// 已经认领的云端身份（`None` = 还没上过云）。
    pub cloud_id: Option<String>,
    pub fingerprint: Option<String>,
    /// 这一款在本机的存档位置 key（用来算"像不像"）。
    pub locations: Vec<String>,
    /// 用户点过「不是同一款」的云端身份：**别再自动绑**（见 [`super::cloud`] 的说明）。
    pub rejected: Vec<String>,
}

/// 云端的一条身份（键 + 卡）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudCard {
    pub key: String,
    pub identity: GameIdentity,
}

/// 这一行是靠什么对上的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// exe 指纹（唯一强判据）。
    Fingerprint,
    /// 名字一模一样。
    Name,
    /// 存档位置 key 有交集。
    Location,
}

/// 已经对上的本机档案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRef {
    pub id: String,
    pub name: String,
    pub evidence: Evidence,
}

/// 配对表上的一行：一条云端身份，以及它在本机的处境。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cloud_key: String,
    pub cloud_id: String,
    pub cloud_name: String,
    /// 云端那边有几台机器认领过它。
    pub machines: usize,
    /// 对上的本机档案。
    pub local: Option<LocalRef>,
    /// 这次扫描**自动绑上**的吗（界面要写明"已自动绑定"，并给一个「不是同一款」）。
    pub auto: bool,
    /// 像、但不敢自动绑的本机档案 id（界面列出来让用户点）。
    pub candidates: Vec<String>,
}

impl Evidence {
    /// 回包里的写法（界面按它选措辞，见 `ui/model/sync.rs` 的 `evidence_label`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Evidence::Fingerprint => "fingerprint",
            Evidence::Name => "name",
            Evidence::Location => "location",
        }
    }
}

impl Row {
    /// 界面用的状态码。
    ///
    /// ⚠ 数值与 `src/ui/slint/types.slint` 的 `PairingItem.state` 一一对应：改这里就
    /// 得改那边（界面上只有一句话的差别，测试也钉着）。
    pub fn state(&self) -> u8 {
        match (self.local.is_some(), self.auto, self.candidates.is_empty()) {
            // 这次扫描自己绑上的：界面要写明依据 + 给一个「不是同一款」。
            (true, true, _) => 1,
            // 早就绑好了。
            (true, false, _) => 0,
            // 有候选，要问。
            (false, _, false) => 2,
            // 本机没有对应的。
            (false, _, true) => 3,
        }
    }
}

/// 扫描的结论：要落盘的绑定 + 给界面看的表。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// 自动绑定的结果（`(本机 id, 云端键, 云端身份)`）—— daemon 直接落盘。
    pub bindings: Vec<(String, String, String)>,
    pub rows: Vec<Row>,
}

/// 扫描一遍，算出该绑什么、该问什么。
pub fn plan(locals: &[LocalGame], clouds: &[CloudCard]) -> Plan {
    let mut plan = Plan::default();

    // 一个指纹在云端**只属于一条身份**时才敢自动绑：两张卡都带着它（身份卡被复制、
    // 或者建了重复的身份）时，绑哪一条都是掷骰子 —— 那就交给用户。
    let mut contenders: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for card in clouds {
        for machine in &card.identity.machines {
            for fingerprint in &machine.fingerprints {
                *contenders.entry(fingerprint.as_str()).or_default() += 1;
            }
        }
    }

    for card in clouds {
        let cloud_id = &card.identity.cloud_id;

        // 1. 已经绑在它上面的档案（本机认领过、或者上一次配对的结果）。
        let bound = locals.iter().find(|local| {
            local
                .cloud_id
                .as_deref()
                .is_some_and(|known| known == cloud_id)
        });
        if let Some(local) = bound {
            plan.rows
                .push(row(card, Some(local_ref(local, card)), false, Vec::new()));
            continue;
        }

        // 2. 指纹命中、而且没被本机别的档案占用：这是唯一敢自己动手的一条。
        //    "没被占用"= 它自己还没认领过身份、也没被本轮别的身份绑走。
        let taken: Vec<&str> = plan
            .bindings
            .iter()
            .map(|(local_id, _, _)| local_id.as_str())
            .collect();
        let hits: Vec<&LocalGame> = locals
            .iter()
            .filter(|local| {
                local.cloud_id.is_none()
                    && !taken.contains(&local.id.as_str())
                    && !local.rejected.iter().any(|id| id == cloud_id)
                    && local.fingerprint.as_ref().is_some_and(|fingerprint| {
                        card.identity.has_fingerprint(fingerprint)
                            && contenders.get(fingerprint.as_str()) == Some(&1)
                    })
            })
            .collect();

        if let [hit] = hits.as_slice() {
            plan.bindings.push((
                hit.id.clone(),
                card.key.clone(),
                card.identity.cloud_id.clone(),
            ));
            plan.rows.push(row(
                card,
                Some(LocalRef {
                    id: hit.id.clone(),
                    name: hit.name.clone(),
                    evidence: Evidence::Fingerprint,
                }),
                true,
                Vec::new(),
            ));
            continue;
        }

        // 3. 剩下的：命中多个、指纹在云端有歧义、或者只靠名字/位置"像" —— 一律列出来
        //    问。候选按判据强弱排：指纹 > 位置对上的多 > 名字。
        let mut alike: Vec<(&LocalGame, u8, usize)> = locals
            .iter()
            .filter(|local| {
                local.cloud_id.is_none()
                    && !taken.contains(&local.id.as_str())
                    && !local.rejected.iter().any(|id| id == cloud_id)
            })
            .filter_map(|local| {
                let shared = local
                    .locations
                    .iter()
                    .filter(|key| {
                        card.identity
                            .machines
                            .iter()
                            .any(|machine| machine.locations.contains(key))
                    })
                    .count();
                let fingerprint_hit = local
                    .fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| card.identity.has_fingerprint(fingerprint));
                if fingerprint_hit {
                    Some((local, 0u8, shared))
                } else if local.name == card.identity.name {
                    Some((local, 1, shared))
                } else if shared > 0 {
                    Some((local, 2, shared))
                } else {
                    None
                }
            })
            .collect();
        alike.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(b.2.cmp(&a.2))
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        let candidates: Vec<String> = alike.iter().map(|(local, _, _)| local.id.clone()).collect();
        plan.rows.push(row(card, None, false, candidates));
    }

    // 表按云端身份的名字排：界面上一眼看得出"云端多了什么"。
    plan.rows.sort_by(|a, b| {
        a.cloud_name
            .cmp(&b.cloud_name)
            .then(a.cloud_id.cmp(&b.cloud_id))
    });
    plan
}

fn row(card: &CloudCard, local: Option<LocalRef>, auto: bool, candidates: Vec<String>) -> Row {
    Row {
        cloud_key: card.key.clone(),
        cloud_id: card.identity.cloud_id.clone(),
        cloud_name: card.identity.name.clone(),
        machines: card.identity.machines.len(),
        local,
        auto,
        candidates,
    }
}

/// 一条云端身份已经绑着某个本机档案时，"靠什么对上的"。
///
/// 绑定本身是既成事实（配置里写着 `cloud_id`），这里的依据只用来在界面上少说一句废话。
fn local_ref(local: &LocalGame, card: &CloudCard) -> LocalRef {
    let evidence = if local
        .fingerprint
        .as_ref()
        .is_some_and(|fingerprint| card.identity.has_fingerprint(fingerprint))
    {
        Evidence::Fingerprint
    } else if local.name == card.identity.name {
        Evidence::Name
    } else {
        Evidence::Location
    };
    LocalRef {
        id: local.id.clone(),
        name: local.name.clone(),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::cloud::MachineIdentity;

    fn card(
        key: &str,
        cloud_id: &str,
        name: &str,
        fingerprints: &[&str],
        locations: &[&str],
    ) -> CloudCard {
        let mut identity = GameIdentity::new(cloud_id, name);
        identity.merge_machine(MachineIdentity {
            machine_id: format!("machine-{cloud_id}"),
            label: "somewhere".to_string(),
            fingerprints: fingerprints.iter().map(|p| p.to_string()).collect(),
            locations: locations.iter().map(|l| l.to_string()).collect(),
            exe_paths: Vec::new(),
        });
        CloudCard {
            key: key.to_string(),
            identity,
        }
    }

    fn local(id: &str, name: &str, fingerprint: Option<&str>) -> LocalGame {
        LocalGame {
            id: id.to_string(),
            name: name.to_string(),
            cloud_id: None,
            fingerprint: fingerprint.map(str::to_string),
            locations: vec!["rel-savedata".to_string()],
            rejected: Vec::new(),
        }
    }

    #[test]
    fn one_fingerprint_hit_binds_itself_and_two_ask() {
        let clouds = vec![card(
            "games/demo",
            "c1",
            "Demo",
            &["v1:10:aa"],
            &["rel-savedata"],
        )];

        // 恰好一个命中：自动绑，并标明依据是指纹。
        let planned = plan(&[local("demo", "Demo", Some("v1:10:aa"))], &clouds);
        assert_eq!(
            planned.bindings,
            vec![(
                "demo".to_string(),
                "games/demo".to_string(),
                "c1".to_string()
            )]
        );
        assert!(planned.rows[0].auto, "自动绑的那一行要说得出来");
        assert_eq!(
            planned.rows[0].local.as_ref().unwrap().evidence,
            Evidence::Fingerprint
        );
        assert!(planned.rows[0].candidates.is_empty());

        // 两个都命中（复制过存档目录、建了两条档案）：**问**，一个都不绑。
        let two = vec![
            local("demo", "Demo", Some("v1:10:aa")),
            local("demo-2", "Demo", Some("v1:10:aa")),
        ];
        let planned = plan(&two, &clouds);
        assert!(planned.bindings.is_empty(), "命中多个绝不自己动手");
        assert!(!planned.rows[0].auto);
        assert_eq!(planned.rows[0].candidates, vec!["demo", "demo-2"]);
    }

    #[test]
    fn a_likeness_that_is_not_a_fingerprint_always_asks() {
        // 名字一样但指纹不同（改过游戏、或者名字撞车）：只能问。
        let clouds = vec![card(
            "games/demo",
            "c1",
            "Demo",
            &["v1:10:aa"],
            &["rel-savedata"],
        )];
        let planned = plan(&[local("demo", "Demo", Some("v1:99:zz"))], &clouds);
        assert!(planned.bindings.is_empty());
        assert_eq!(planned.rows[0].candidates, vec!["demo"]);

        // 指纹对不上、名字也不一样，但存档位置有交集：也算"像"（问）。
        let other = LocalGame {
            name: "别的名字".to_string(),
            ..local("other", "别的名字", Some("v1:99:zz"))
        };
        let planned = plan(&[other], &clouds);
        assert_eq!(planned.rows[0].candidates, vec!["other"]);

        // 什么都不像：一条候选都没有，界面只把它列出来。
        let stranger = LocalGame {
            name: "完全无关".to_string(),
            locations: vec!["abs-elsewhere".to_string()],
            ..local("stranger", "完全无关", Some("v1:99:zz"))
        };
        let planned = plan(&[stranger], &clouds);
        assert!(planned.rows[0].candidates.is_empty());
        assert!(planned.rows[0].local.is_none());
    }

    #[test]
    fn an_identity_already_bound_stays_bound() {
        let clouds = vec![card("games/demo", "c1", "Demo", &["v1:10:aa"], &[])];
        let bound = LocalGame {
            cloud_id: Some("c1".to_string()),
            ..local("demo", "Demo", Some("v1:10:aa"))
        };
        let planned = plan(&[bound], &clouds);
        assert!(planned.bindings.is_empty(), "已经绑好了，不用再绑一次");
        assert!(!planned.rows[0].auto);
        assert_eq!(planned.rows[0].local.as_ref().unwrap().id, "demo");
        assert_eq!(
            planned.rows[0].local.as_ref().unwrap().evidence,
            Evidence::Fingerprint
        );
        assert!(planned.rows[0].candidates.is_empty());
    }

    #[test]
    fn a_rejected_pairing_is_never_suggested_again() {
        let clouds = vec![card(
            "games/demo",
            "c1",
            "Demo",
            &["v1:10:aa"],
            &["rel-savedata"],
        )];
        let rejected = LocalGame {
            rejected: vec!["c1".to_string()],
            ..local("demo", "Demo", Some("v1:10:aa"))
        };
        let planned = plan(&[rejected], &clouds);
        assert!(planned.bindings.is_empty(), "用户说过不是同一款了");
        assert!(planned.rows[0].candidates.is_empty(), "连候选都不该再列");
    }

    #[test]
    fn a_local_game_already_claimed_elsewhere_is_not_stolen() {
        // 两个云端身份都带同一个指纹（身份卡复制过）：本机档案只能绑一个，
        // 于是两条都变成"问"，谁也不许抢。
        let clouds = vec![
            card("games/one", "c1", "One", &["v1:10:aa"], &[]),
            card("games/two", "c2", "Two", &["v1:10:aa"], &[]),
        ];
        let planned = plan(&[local("demo", "Demo", Some("v1:10:aa"))], &clouds);
        assert!(planned.bindings.is_empty());
        assert!(
            planned
                .rows
                .iter()
                .all(|row| row.candidates == vec!["demo"])
        );
    }
}

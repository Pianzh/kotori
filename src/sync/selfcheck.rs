//! 打开游戏前的自检：**只有一种情况会打断用户**。
//!
//! 全流程（用户 2026-09-22："云同步（打开游戏）前必须自检"）：
//!
//! 1. 目标没配齐 / 这一款的开关关着 ⇒ 什么都不做，直接启动（**自检失败绝不拦启动**）。
//! 2. 这一款在**当前目标**上已确认（结论里的签名与当前签名一致）⇒ 不重扫、不问，
//!    直接取回较新的存档（防错配闸照旧生效）。
//! 3. 未定 ⇒ 先按指纹在**当前桶**里找一次：恰好命中一条 ⇒ 静默认领并盖章；否则问一次。
//!
//! 这个模块是**纯决策**：看云端那一步由调用方给一个闭包（`lookup`），而且**只在第 3 步
//! 才调用** —— kopia 那边"读一次身份 = 一次 restore"，已确认的那条路上一个字都不该读。

use crate::config::GameConfig;
use crate::sync::signature::Conclusion;

/// 云端一条身份里**给用户看的那几栏**（弹窗里"疑似找到的那一条"）。
///
/// ⚠ 与界面的 `CloudIdentityLabel` 一一对应：daemon 只报事实，**名字与摘要由界面用同一个
/// 函数生成**（用户 2026-09-24："弹窗显示的近似游戏信息使用的是和设置页面给出信息一样的
/// 函数就可以了，方便后期统一修改"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudPeek {
    pub cloud_id: String,
    pub cloud_key: String,
    pub name: String,
    pub versions: u64,
    pub latest: String,
    pub size: u64,
}

/// 指纹在当前云目标上找到了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// 没命中（包括指纹还没算出来）。
    None,
    /// 恰好命中一条，而且这条身份还没被本机别的档案认领。
    One { cloud_id: String, cloud_key: String },
    /// 命中多条，或者命中的那条已经被本机别的档案占着 —— 都要问。带上的那几条是给弹窗挑
    /// "最像的一条"用的（规则在 [`crate::sync::matching::best_like`] 那一个函数里）。
    Many(Vec<CloudPeek>),
}

/// 自检的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 不打扰：直接启动。
    Skip,
    /// 在当前目标上已确认：照常取回。
    Pull,
    /// 指纹恰好命中一条：静默认领并盖章，然后取回。
    Adopt { cloud_id: String, cloud_key: String },
    /// 不再问：直接新建一条身份（用户自己把"关掉过"的那款开关重新打开了）。
    Fresh,
    /// 问一次。`found` = **疑似找到的那一条**；`None` = 完全没找到，界面照实说、
    /// 让你自己挑。
    Ask { found: Option<CloudPeek> },
}

/// 要不要为了这次自检去**读云端**（kopia 那边读一次身份 = 一次 restore）。
///
/// 开关关着、目标没配齐、已确认、没有指纹 —— 这四种都不用读。[`decide`] 与它必须
/// 说同一件事：调用方先问这个，再决定要不要花那次网络往返。
pub fn needs_cloud(game: &GameConfig, signature: Option<&str>) -> bool {
    let (Some(signature), true) = (signature, game.sync_enabled) else {
        return false;
    };
    if confirmed_on(game, signature) {
        return false;
    }
    game.exe_fingerprint
        .as_deref()
        .is_some_and(|f| !f.is_empty())
}

/// 这一款在当前目标上**真的**确认过没有？
///
/// ⚠ 除了签名要对得上，**还必须绑着一条身份**。用户 2026-09-24 报过"没有存档，云同步
/// 点开，但是没有弹出未命中窗口" —— 那正是"结论说确认过、其实没绑"的那一款：没绑的确认
/// 没有落到实处，点启动还是要自检一次（认得出就自动绑上，认不出就问）。
fn confirmed_on(game: &GameConfig, signature: &str) -> bool {
    game.cloud_id.is_some()
        && matches!(
            game.cloud_conclusion.as_deref().and_then(Conclusion::parse),
            Some(Conclusion::Confirmed(known)) if known == signature
        )
}

/// 自检。`lookup` 只在真的需要看云端时被调用一次。
pub fn decide<F>(game: &GameConfig, signature: Option<&str>, lookup: F) -> Decision
where
    F: FnOnce() -> Found,
{
    // 1. 目标没配齐、或者这一款的开关关着：一个字都不做。
    //    （"关掉这一款"这条路上，用户下次打开游戏不该再被问第二次。）
    let (Some(signature), true) = (signature, game.sync_enabled) else {
        return Decision::Skip;
    };
    let conclusion = game.cloud_conclusion.as_deref().and_then(Conclusion::parse);

    // 2. 在当前目标上**真的**确认过（签名对得上，而且绑着一条身份）：不重扫、不问。
    if confirmed_on(game, signature) {
        return Decision::Pull;
    }

    // 3. 问过一次、答案是"关掉这一款"或"以后新建一条"，而且是在**当前这个目标**上问的：
    //    云端本来就没有它在等的身份，不必再扫一遍 —— 直接按"新建"继续。
    //
    //    ⚠ 必须在扫云端**之前**：省下的不只是一次弹窗，还有每次启动都要做的那一次
    //    云端索引读取（缓存过期时就是一次下载）—— 用户 2026-09-26 报的"卡在启动中"
    //    就发生在那一段之后，而它每次都白白重来一遍。
    if matches!(
        conclusion,
        Some(Conclusion::New(known) | Conclusion::Declined(known)) if known == signature
    ) {
        return Decision::Fresh;
    }

    // 4. 仍未定：按指纹在当前桶里找一次（这是唯一会读云端的一步）。
    if needs_cloud(game, Some(signature)) {
        match lookup() {
            Found::One {
                cloud_id,
                cloud_key,
            } => {
                return Decision::Adopt {
                    cloud_id,
                    cloud_key,
                };
            }
            // 命中多条：弹窗里只显示**最像的那一条**（规则在 `matching::best_like`，
            // 现在是取第一条）。一条都没带（"唯一那条被本机别的档案占着"）就是 `None`。
            Found::Many(candidates) => {
                let found = crate::sync::matching::best_like(&candidates).cloned();
                return ask_or_fresh(conclusion.as_ref(), found);
            }
            Found::None => {}
        }
    }

    ask_or_fresh(conclusion.as_ref(), None)
}

/// 最后一步：问一次 —— **除非**他上次就答过"关掉这一款"或"以后新建一条"（那两次都已经
/// 问过了，用户原话："如果匹配不上还强制打开就建立新游戏存档位置"）。第 3 步先挡掉了
/// 当前目标上的那种结论，这里再兜一次"换了目标、结论里那个签名对不上"的边角。
///
/// `found` 是"疑似找到的那一条"（没有就是完全没找到）—— 界面据此分两种说法，用户
/// 2026-09-24："直接把找到像的和没找到像的打包成函数或者条件，分别显示疑似找到和完全
/// 没找到两个 ui"。
fn ask_or_fresh(conclusion: Option<&Conclusion>, found: Option<CloudPeek>) -> Decision {
    match conclusion {
        // 问过一次、答案是"关掉这一款"或者"以后新建一条"：都不再问，直接新建身份。
        Some(Conclusion::Declined(_)) | Some(Conclusion::New(_)) => Decision::Fresh,
        _ => Decision::Ask { found },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> GameConfig {
        serde_json::from_value(serde_json::json!({
            "name": "demo",
            "exe_path": "/games/demo/game.exe",
            "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    const SIG: &str = "v1:kopia::bkt:kotori";
    const OTHER: &str = "v1:kopia::other:kotori";

    fn hit(cloud_id: &str, key: &str) -> Found {
        Found::One {
            cloud_id: cloud_id.to_string(),
            cloud_key: key.to_string(),
        }
    }

    #[test]
    fn a_game_whose_switch_is_off_is_never_asked_about() {
        let mut game = game();
        game.sync_enabled = false;
        game.exe_fingerprint = Some("v1:1:aa".into());
        assert_eq!(decide(&game, Some(SIG), || hit("x", "y")), Decision::Skip);
    }

    #[test]
    fn no_target_means_nothing_to_do() {
        let game = game();
        assert_eq!(decide(&game, None, || Found::None), Decision::Skip);
    }

    /// ⚠ "确认过"必须**绑着一条身份**才算数（用户 2026-09-24 报的场景：结论说确认过、
    /// 其实没绑 ⇒ 点启动什么都不问）。没绑的那种见下一条测试。
    #[test]
    fn a_confirmed_game_is_not_scanned_again() {
        let mut game = game();
        game.cloud_id = Some("cloud-1".into());
        game.cloud_conclusion = Some(Conclusion::confirmed(SIG));
        game.exe_fingerprint = Some("v1:1:aa".into());
        // `lookup` 一被调用就炸：已确认那条路上一个字都不许读云端。
        assert_eq!(
            decide(&game, Some(SIG), || panic!("不该去看云端")),
            Decision::Pull
        );
    }

    /// "确认过、但没绑"不算数：点启动还是要自检 —— 认得出就自动绑上，认不出就问。
    #[test]
    fn a_confirmation_without_an_identity_does_not_count() {
        let mut game = game();
        game.cloud_conclusion = Some(Conclusion::confirmed(SIG));
        game.exe_fingerprint = Some("v1:1:aa".into());
        assert!(
            needs_cloud(&game, Some(SIG)),
            "没绑的确认没有落到实处，该去看一眼"
        );
        assert_eq!(
            decide(&game, Some(SIG), || hit("cloud-1", "demo")),
            Decision::Adopt {
                cloud_id: "cloud-1".into(),
                cloud_key: "demo".into()
            }
        );
        assert_eq!(
            decide(&game, Some(SIG), || Found::None),
            Decision::Ask { found: None }
        );
    }

    #[test]
    fn another_target_makes_the_conclusion_stale_and_the_fingerprint_decides() {
        let mut game = game();
        game.cloud_conclusion = Some(Conclusion::confirmed(OTHER));
        game.exe_fingerprint = Some("v1:1:aa".into());
        assert_eq!(
            decide(&game, Some(SIG), || hit("cloud-1", "demo")),
            Decision::Adopt {
                cloud_id: "cloud-1".into(),
                cloud_key: "demo".into()
            }
        );
        // 换了桶又认不出来：问一次（这次他还没答过"关掉"），而且**不带**"疑似找到的那
        // 一条" —— 完全没找到就不编名字。
        assert_eq!(
            decide(&game, Some(SIG), || Found::None),
            Decision::Ask { found: None }
        );
        assert_eq!(
            decide(&game, Some(SIG), || Found::Many(Vec::new())),
            Decision::Ask { found: None }
        );
    }

    #[test]
    fn a_game_without_a_fingerprint_is_asked_instead_of_guessed() {
        let game = game();
        assert_eq!(
            decide(&game, Some(SIG), || panic!("没有指纹就不该去查")),
            Decision::Ask { found: None }
        );
    }

    /// 用户答过"关掉这一款"，之后自己又把开关打开：**不再问**，直接新建身份。
    ///
    /// ⚠ 界面上那颗开关现在会**顺手清掉结论**（见 `game_rpc::rpc_game_update`），所以这条路
    /// 主要服务"结论还在、开关已经被别的途径打开"的情形（老配置、CLI 直接改配置）。
    #[test]
    fn re_enabling_a_game_that_was_switched_off_creates_a_new_identity_silently() {
        let mut game = game();
        game.sync_enabled = true;
        game.cloud_conclusion = Some(Conclusion::declined(SIG));
        game.exe_fingerprint = Some("v1:1:aa".into());
        assert_eq!(decide(&game, Some(SIG), || Found::None), Decision::Fresh);
        // 认得出旧身份就还是认领它 —— "匹配不上才新建"。
        assert_eq!(
            decide(&game, Some(SIG), || hit("cloud-9", "demo")),
            Decision::Adopt {
                cloud_id: "cloud-9".into(),
                cloud_key: "demo".into()
            }
        );
    }

    /// 用户答过"以后新建一条"：**下次启动不再问**，而且**连云端都不去看**。
    ///
    /// ⚠ 这是 2026-09-26 那个 bug 的看门测试：写结论的那一端（`apply_decision`）曾经把
    /// "新建"盖成 `ok:<签名>`（＝"已确认、而且绑着身份"）—— 而身份刚被清空，于是每次
    /// 启动都重走一遍查云端、再弹一次窗，用户永远建不出档案。
    #[test]
    fn a_game_that_answered_new_is_not_asked_again_nor_scanned() {
        let mut game = game();
        game.sync_enabled = true;
        game.cloud_conclusion = Some(Conclusion::fresh(SIG));
        game.exe_fingerprint = Some("v1:1:aa".into());
        // `lookup` 一被调用就炸：答过"新建"之后一个字都不该读云端。
        assert_eq!(
            decide(&game, Some(SIG), || panic!("答过'新建'就不该再去读云端")),
            Decision::Fresh
        );

        // 换了一个桶：上一次那个结论不作数，该重新看一眼云端（哪怕最后还是走"新建"）。
        let looked = std::cell::Cell::new(false);
        assert_eq!(
            decide(&game, Some(OTHER), || {
                looked.set(true);
                Found::None
            }),
            Decision::Fresh
        );
        assert!(looked.get(), "换了目标就该重新查一次");
    }

    /// 指纹命中多条：问一次，并把**最像的那一条**带上（现在是取第一条，规则在
    /// `matching::best_like`）—— 界面据此显示"疑似找到"。
    #[test]
    fn many_hits_ask_once_and_carry_the_best_one() {
        let mut game = game();
        game.exe_fingerprint = Some("v1:1:aa".into());
        let peek = |cloud_id: &str, name: &str| CloudPeek {
            cloud_id: cloud_id.to_string(),
            cloud_key: format!("games/{cloud_id}"),
            name: name.to_string(),
            versions: 3,
            latest: "20260911T101500Z".to_string(),
            size: 4096,
        };
        assert_eq!(
            decide(&game, Some(SIG), || Found::Many(vec![
                peek("c1", "一号"),
                peek("c2", "二号"),
            ])),
            Decision::Ask {
                found: Some(peek("c1", "一号"))
            },
            "带上的必须是第一条（`best_like` 现在的规则）"
        );
    }

    #[test]
    fn a_fingerprint_hit_wins_over_an_unrelated_conclusion() {
        let mut game = game();
        game.cloud_conclusion = Some("看不懂的东西".into());
        game.exe_fingerprint = Some("v1:1:aa".into());
        assert_eq!(
            decide(&game, Some(SIG), || hit("cloud-2", "renamed")),
            Decision::Adopt {
                cloud_id: "cloud-2".into(),
                cloud_key: "renamed".into()
            }
        );
    }
}

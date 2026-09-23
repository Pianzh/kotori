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

/// 指纹在当前云目标上找到了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// 没命中（包括指纹还没算出来）。
    None,
    /// 恰好命中一条，而且这条身份还没被本机别的档案认领。
    One { cloud_id: String, cloud_key: String },
    /// 命中多条，或者命中的那条已经被本机别的档案占着 —— 都要问。
    Many,
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
    /// 问一次。
    Ask,
}

/// 要不要为了这次自检去**读云端**（kopia 那边读一次身份 = 一次 restore）。
///
/// 开关关着、目标没配齐、已确认、没有指纹 —— 这四种都不用读。[`decide`] 与它必须
/// 说同一件事：调用方先问这个，再决定要不要花那次网络往返。
pub fn needs_cloud(game: &GameConfig, signature: Option<&str>) -> bool {
    let (Some(signature), true) = (signature, game.sync_enabled) else {
        return false;
    };
    let conclusion = game.cloud_conclusion.as_deref().and_then(Conclusion::parse);
    if matches!(conclusion, Some(Conclusion::Confirmed(known)) if known == signature) {
        return false;
    }
    game.exe_fingerprint
        .as_deref()
        .is_some_and(|f| !f.is_empty())
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

    // 2. 在当前目标上已经确认过：不重扫、不问。
    if matches!(conclusion, Some(Conclusion::Confirmed(known)) if known == signature) {
        return Decision::Pull;
    }

    // 3. 未定：按指纹在当前桶里找一次（这是唯一会读云端的一步）。
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
            Found::None | Found::Many => {}
        }
    }

    // 4. 没命中 / 命中多条：问一次 —— **除非**他上次就是答"关掉这一款"（那次已经问过
    //    了，用户原话："如果匹配不上还强制打开就建立新游戏存档位置"）。
    if matches!(conclusion, Some(Conclusion::Declined(_))) {
        Decision::Fresh
    } else {
        Decision::Ask
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

    #[test]
    fn a_confirmed_game_is_not_scanned_again() {
        let mut game = game();
        game.cloud_conclusion = Some(Conclusion::confirmed(SIG));
        game.exe_fingerprint = Some("v1:1:aa".into());
        // `lookup` 一被调用就炸：已确认那条路上一个字都不许读云端。
        assert_eq!(
            decide(&game, Some(SIG), || panic!("不该去看云端")),
            Decision::Pull
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
        // 换了桶又认不出来：问一次（这次他还没答过"关掉"）。
        assert_eq!(decide(&game, Some(SIG), || Found::None), Decision::Ask);
        assert_eq!(decide(&game, Some(SIG), || Found::Many), Decision::Ask);
    }

    #[test]
    fn a_game_without_a_fingerprint_is_asked_instead_of_guessed() {
        let game = game();
        assert_eq!(
            decide(&game, Some(SIG), || panic!("没有指纹就不该去查")),
            Decision::Ask
        );
    }

    /// 用户答过"关掉这一款"，之后自己又把开关打开：**不再问**，直接新建身份。
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

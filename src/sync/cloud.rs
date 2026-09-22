//! 云端有哪几款游戏：跨机器可见性的那一层。
//!
//! 从前 kotori 只会在**已知 id** 的目录下列版本（`games/<id>/`），从来没有列过
//! `games/` 那一层 —— 于是第二台机器发现不了云端有什么，只有"两台机器给同一款
//! 游戏起的名字一字不差"时才能碰巧对上。这个模块补的就是"先问云端有哪几款"。
//!
//! 它是**云同步身份**那件事的第一步：先看得见（这里），再谈"哪一款对应哪一款"
//! （exe 指纹、身份卡、配对界面）。两条路在这一层说同一句话：
//!
//!   * rclone：`games/` 下的**目录名**就是一个游戏（一版一个 zip 摆在里面）；
//!   * kopia：整个仓库是不透明的一块，能认人的只有快照上的 `game:` **标签值**。
//!
//! 于是"云端这一款的标识"在两个引擎下都叫 [`CloudGame::id`]：rclone 那边它同时
//! 也是目录名，kopia 那边它就是标签值。第 3 步之后 rclone 的目录名可能为了避开
//! 占用而带后缀，那时"目录名"与"身份"才需要分开说。

use serde::{Deserialize, Serialize};

/// 云端的一款游戏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudGame {
    /// 云端这一款的标识（rclone 是目录名，kopia 是 `game:` 标签的值）。
    pub id: String,
    /// 有几版存档。kopia 那条**身份快照**不算版本，rclone 的身份卡也不是包。
    pub versions: usize,
}

/// 一版存档随身带着的身份（写在包清单里，kopia 那边写在同一条快照的清单里）。
///
/// 判"云端这一版是不是本机这一款"只能靠它：包里的文件和路径都是**本机**的样子，
/// 而两台机器给同一款游戏起的 id 可能不同、同一个 id 也可能是两款不同的游戏
/// （slug 归一化）。所以"哪个对应哪个"必须由身份回答，不能由名字回答。
///
/// 第 2 步（现在）要的是 `cloud_id`：**不一致就绝不铺到本机存档上**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackIdentity {
    /// 这款游戏在云端的身份（`GameConfig::cloud_id`）。
    pub cloud_id: String,
    /// 传这一版的是哪台机器（`DaemonConfig::machine_id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// 这一款在这台机器上的 exe 指纹（第 3 步才算得出来，现在多半是空的）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// 这台机器上这一款配了哪些存档位置（`save_key`），跨机器的位置对齐要用（§2.7）。
    #[serde(default)]
    pub locations: Vec<String>,
}

/// 身份用来给人看的那一小段。
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 云端那一版与本机这一款的对照结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityMatch {
    /// 同一个身份：云端这一版就是本机这一款的。
    Same,
    /// 两边都是身份，但**不是同一个** —— 云端那一版是别的一款（或别的一台机器的）。
    Different { cloud: String, local: String },
    /// 缺一头：本机这一款还没认领过身份，或者云端那一版压根没有身份段（更早的
    /// kotori 传的）。**不猜。**
    Unpaired { cloud_has_identity: bool },
}

impl IdentityMatch {
    /// 不能动它时该对用户说的话；[`IdentityMatch::Same`] 时是 `None`。
    ///
    /// 两句都点明"什么也没动"，因为这条路的另一端是用户的存档：静默跳过会让人
    /// 以为同步过了。
    pub fn refusal(&self) -> Option<String> {
        match self {
            IdentityMatch::Same => None,
            IdentityMatch::Different { cloud, local } => Some(format!(
                "云端这一版是另一个身份的存档（{}），本机这一款是 {} —— 已跳过，本机存档一个都没动",
                short_id(cloud),
                short_id(local)
            )),
            IdentityMatch::Unpaired {
                cloud_has_identity: true,
            } => Some(
                "云端这一版带着身份，而本机这一款还没有云端身份（本机第一次上传时才会有）\
                 —— 已跳过：宁可不动，也不猜"
                    .to_string(),
            ),
            IdentityMatch::Unpaired {
                cloud_has_identity: false,
            } => Some(
                "云端这一版里没有身份信息（比本机这个版本更早的 kotori 传的）—— 已跳过".to_string(),
            ),
        }
    }
}

/// 判"云端这一版是不是本机这一款"。
///
/// 三种答案的处置不同（见 [`IdentityMatch::refusal`]），但**只有 [`IdentityMatch::Same`]
/// 才允许碰本机存档**：自动取回与手动恢复共用这一条（区别只在手动那边用户是明确的
/// 发起人，仍然不该被猜出来的身份覆盖）。
pub fn identity_match(local: Option<&str>, remote: Option<&PackIdentity>) -> IdentityMatch {
    match (local, remote) {
        (Some(local), Some(remote)) if local == remote.cloud_id => IdentityMatch::Same,
        (Some(local), Some(remote)) => IdentityMatch::Different {
            cloud: remote.cloud_id.clone(),
            local: local.to_string(),
        },
        (None, Some(_)) => IdentityMatch::Unpaired {
            cloud_has_identity: true,
        },
        (Some(_), None) | (None, None) => IdentityMatch::Unpaired {
            cloud_has_identity: false,
        },
    }
}

/// 解析 `rclone lsf --dirs-only` 的输出：一行一个目录名。
///
/// rclone 默认给目录名加尾斜杠（`--dir-slash`），去掉它才是能用进路径的名字。
/// 空行与重复项一并收掉：`games/` 那一层不该有空白名字，而重名的目录本就不存在
/// ——真要出现，宁可只报一次也不要让界面上出现两行一样的东西。
pub fn parse_dirs(output: &str) -> Vec<String> {
    let mut dirs: Vec<String> = output
        .lines()
        .map(|line| line.trim().trim_end_matches('/'))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_listings_become_usable_names() {
        let output = "3days/\nlife-game/\n\nlong-name/\n3days/\n";
        assert_eq!(
            parse_dirs(output),
            vec![
                "3days".to_string(),
                "life-game".to_string(),
                "long-name".to_string()
            ],
            "尾斜杠要去掉，空行与重复项都不留"
        );
        assert!(parse_dirs("").is_empty(), "云端还没有游戏时是空的");
        // 没带尾斜杠也认（老 rclone、或者有人 `--dir-slash=false`）。
        assert_eq!(parse_dirs("solo\n"), vec!["solo".to_string()]);
    }

    fn identity(cloud_id: &str) -> PackIdentity {
        PackIdentity {
            cloud_id: cloud_id.to_string(),
            machine_id: Some("machine-1".to_string()),
            fingerprint: None,
            locations: vec!["rel-savedata".to_string()],
        }
    }

    #[test]
    fn only_the_same_identity_may_touch_local_saves() {
        // 同一个身份：唯一一种敢动本机存档的情形。
        assert_eq!(
            identity_match(Some("abc"), Some(&identity("abc"))),
            IdentityMatch::Same
        );

        // 两个身份不一样 = 云端那一版是别的一款。这是**静默损坏存档**那条路，
        // 必须拒绝，而且说清楚"本机一个都没动"。
        let mismatch = identity_match(Some("abc"), Some(&identity("xyz")));
        assert_eq!(
            mismatch,
            IdentityMatch::Different {
                cloud: "xyz".to_string(),
                local: "abc".to_string()
            }
        );
        let refusal = mismatch.refusal().unwrap();
        assert!(refusal.contains("已跳过"), "{refusal}");
        assert!(refusal.contains("本机存档一个都没动"), "{refusal}");
        // 只露前 8 位：身份是 uuid，界面上不必看全。
        assert!(!refusal.contains("xyz1234567890"), "{refusal}");

        // 缺一头都不猜：本机还没认领身份 / 云端那一版没有身份段。
        for (local, remote, cloud_has) in [
            (None, Some(identity("abc")), true),
            (Some("abc"), None, false),
            (None, None, false),
        ] {
            let verdict = identity_match(local, remote.as_ref());
            assert_eq!(
                verdict,
                IdentityMatch::Unpaired {
                    cloud_has_identity: cloud_has
                }
            );
            let refusal = verdict.refusal().unwrap();
            assert!(refusal.contains("已跳过"), "{refusal}");
        }
        assert!(IdentityMatch::Same.refusal().is_none());
    }

    #[test]
    fn an_identity_survives_the_round_trip_into_a_manifest() {
        let identity = PackIdentity {
            cloud_id: "8f2c-…".to_string(),
            machine_id: Some("1a2b".to_string()),
            fingerprint: Some("v1:184320000:9f3c".to_string()),
            locations: vec!["rel-savedata".to_string(), "win-appdata_game".to_string()],
        };
        let text = serde_json::to_string(&identity).unwrap();
        assert_eq!(
            serde_json::from_str::<PackIdentity>(&text).unwrap(),
            identity
        );
        // 老包（没有身份段）读出来是 `None`，不是"解析失败"。
        assert_eq!(
            serde_json::from_str::<Option<PackIdentity>>("null").unwrap(),
            None
        );
        assert_eq!(short_id("8f2c1234567890"), "8f2c1234");
    }
}

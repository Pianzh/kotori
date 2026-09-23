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

/// 身份卡的文件名：rclone 那边就摆在 `games/<目录>/` 里，与包并排。
pub const IDENTITY_FILE: &str = "kotori-game.json";
/// 身份卡的格式版本。结构变了就换它（老卡当成"读不懂"，不当成"能猜"）。
pub const IDENTITY_FORMAT: u32 = 1;
/// kopia 那边身份快照的 description。
///
/// ⚠ **必须不像我们自己的版本名**：`versions()` 与保留窗口都是靠"名字长得像不像"
/// 认出我们自己的快照的，一句固定的话才不会被当成一版存档（§5.3）。
pub const IDENTITY_DESCRIPTION: &str = "kotori-identity";
/// 快照标签 `kind` 的两个取值：存档 / 身份。
///
/// 同一个仓库里两种快照混着放，凡是"列版本、算保留窗口"的地方都只许看
/// [`KIND_SAVE`] 那一种（§5.3）。
pub const KIND_SAVE: &str = "save";
pub const KIND_IDENTITY: &str = "identity";
/// 索引（`crate::sync::index`）：一个桶一份的"云端现在有什么"。
pub const KIND_INDEX: &str = "index";

/// 云端的一款游戏的身份（`kotori-game.json`）。
///
/// 两个引擎存同一份东西，只是"存在哪儿"不同：rclone 是桶里一个 json 文件，kopia 是
/// 一条**只装着这个 json** 的快照。它回答的是本机档案回答不了的那个问题：**两台机器
/// 上哪两条档案是同一款游戏**。
///
/// **绝不放**绝对路径、密钥、存档内容：`locations` 是 `save_key` 的产物（`rel-savedata`
/// 这种），指纹是 exe 的哈希，仅此而已。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameIdentity {
    /// 格式版本（[`IDENTITY_FORMAT`]）。
    pub format: u32,
    /// 这一款在云端的身份：与本机路径、游戏名都无关。
    pub cloud_id: String,
    /// 给人看的名字（本机建档时的游戏名，只作参考，**不参与任何判断**）。
    #[serde(default)]
    pub name: String,
    /// 这份身份是什么时候建/最后更新的（RFC3339，只给人看）。
    #[serde(default)]
    pub created: String,
    /// 认领过这一款的机器，一台一条。
    #[serde(default)]
    pub machines: Vec<MachineIdentity>,
}

/// 一台机器为一款游戏留下的东西。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub machine_id: String,
    /// 机器名，只给人看（主机名会变，所以它绝不参与判断）。
    #[serde(default)]
    pub label: String,
    /// 这台机器上这款游戏见过的 exe 指纹（只追加，见 §5.5）。
    #[serde(default)]
    pub fingerprints: Vec<String>,
    /// 这台机器上这款游戏配了哪些存档位置（`save_key`），用来做位置对齐（§2.7）。
    #[serde(default)]
    pub locations: Vec<String>,
    /// 这台机器上这款游戏**用过的 exe 路径**。
    ///
    /// ⚠ **只作参考信息与搜索参数**（用户 2026-09-23："以前的不上云只是我们不根据目录来
    /// 判断本机目录而已，这次的 exe 路径仅仅只做信息参考使用"）。所以它**绝不参与任何
    /// 判断**（谁是谁只看指纹）、**绝不写回本机配置**、**绝不拿来还原本机目录**。
    /// 它在桶里，是为了让云端存档页能按"我记得那个 exe 叫啥"搜到。
    #[serde(default)]
    pub exe_paths: Vec<String>,
}

impl GameIdentity {
    /// 新建一份身份（第一次上传、或者云端那一款还没有身份卡时）。
    pub fn new(cloud_id: &str, name: &str) -> Self {
        Self {
            format: IDENTITY_FORMAT,
            cloud_id: cloud_id.to_string(),
            name: name.to_string(),
            created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            machines: Vec::new(),
        }
    }

    /// 这个指纹是不是这一款的？（"云端那一款是不是我这台机器上这一款"的判据。）
    pub fn has_fingerprint(&self, fingerprint: &str) -> bool {
        self.machines.iter().any(|machine| {
            machine
                .fingerprints
                .iter()
                .any(|print| print == fingerprint)
        })
    }

    /// 把"自己这台机器"并进来：**只追加**，绝不改别人的、也绝不删。
    ///
    /// 两台机器同时上传时可能各读到一份旧卡、各写回一份新的，后写的会盖掉先写的
    /// （§5.5）。所以这里的原则是"丢了下次上传再补"，而不是"谁最后写谁说了算"。
    pub fn merge_machine(&mut self, machine: MachineIdentity) {
        match self
            .machines
            .iter_mut()
            .find(|known| known.machine_id == machine.machine_id)
        {
            Some(known) => {
                // 机器名会变（改主机名、重装）：以这一次报上来的为准。
                known.label = machine.label;
                for print in machine.fingerprints {
                    if !known.fingerprints.contains(&print) {
                        known.fingerprints.push(print);
                    }
                }
                for location in machine.locations {
                    if !known.locations.contains(&location) {
                        known.locations.push(location);
                    }
                }
                // 用过的 exe 路径：与指纹同样**只追加**（换过 exe、装过别处都要记得住，
                // 它只是个搜索参数，多一点没坏处）。
                for path in machine.exe_paths {
                    if !known.exe_paths.contains(&path) {
                        known.exe_paths.push(path);
                    }
                }
            }
            None => self.machines.push(machine),
        }
    }

    /// 在云端的身份卡里按指纹找候选（0 个 = 云端没有它，1 个 = 就是它，≥2 个 = **要问**）。
    ///
    /// 入参是"键 + 卡"：找到之后要跟它走**同一个键**（rclone 是目录名），版本才会落
    /// 在同一处 —— 两台机器的游戏名不一样时，全靠这个把包放进同一个目录。
    pub fn find_by_fingerprint<'a>(
        cards: &'a [(String, GameIdentity)],
        fingerprint: &str,
    ) -> Vec<&'a (String, GameIdentity)> {
        cards
            .iter()
            .filter(|(_, identity)| identity.has_fingerprint(fingerprint))
            .collect()
    }
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

/// 身份用来给人看（或当目录名后缀）的那一小段。
///
/// 长度由调用方定：给人看的取 8 位足够认出来，当目录名后缀取 6 位（§5.7）。
pub fn short_id(id: &str, len: usize) -> String {
    id.chars().take(len).collect()
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
                short_id(cloud, 8),
                short_id(local, 8)
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
        assert_eq!(short_id("8f2c1234567890", 8), "8f2c1234");
        assert_eq!(
            short_id("8f2c1234567890", 6),
            "8f2c12",
            "目录名后缀只要 6 位"
        );
    }

    fn machine(machine_id: &str, prints: &[&str]) -> MachineIdentity {
        MachineIdentity {
            machine_id: machine_id.to_string(),
            label: format!("host-{machine_id}"),
            fingerprints: prints.iter().map(|p| p.to_string()).collect(),
            locations: vec!["rel-savedata".to_string()],
            exe_paths: vec![format!("/games/{machine_id}/game.exe")],
        }
    }

    #[test]
    fn an_identity_keeps_one_entry_per_machine_and_only_appends() {
        let mut identity = GameIdentity::new("cloud-1", "示例游戏");
        assert_eq!(identity.format, IDENTITY_FORMAT);
        assert!(identity.machines.is_empty());

        identity.merge_machine(machine("machine-a", &["v1:10:aa"]));
        identity.merge_machine(machine("machine-b", &["v1:20:bb"]));
        assert_eq!(identity.machines.len(), 2);

        // 同一台机器再来一次：**只追加**，不重复、不覆盖别人的。
        let mut again = machine("machine-a", &["v1:10:aa", "v1:30:cc"]);
        again.label = "renamed-host".to_string();
        identity.merge_machine(again);
        assert_eq!(identity.machines.len(), 2, "一台机器一条");
        assert_eq!(identity.machines[0].label, "renamed-host", "机器名会变");
        assert_eq!(
            identity.machines[0].fingerprints,
            vec!["v1:10:aa".to_string(), "v1:30:cc".to_string()],
            "重复的不再写一遍，新的追加在后面"
        );
        assert_eq!(
            identity.machines[1].fingerprints,
            vec!["v1:20:bb".to_string()],
            "别人的指纹一个都没动"
        );

        // 用过的 exe 路径与指纹同一条规矩：只追加，不重复，也不动别人的。
        let mut moved = machine("machine-a", &["v1:10:aa"]);
        moved.exe_paths = vec![
            "/games/machine-a/game.exe".to_string(),
            "/mnt/games/elsewhere/game.exe".to_string(),
        ];
        identity.merge_machine(moved);
        assert_eq!(
            identity.machines[0].exe_paths,
            vec![
                "/games/machine-a/game.exe".to_string(),
                "/mnt/games/elsewhere/game.exe".to_string()
            ],
            "新路径追加在后面，已有的不再写一遍"
        );
        assert_eq!(identity.machines[1].exe_paths.len(), 1);
    }

    #[test]
    fn a_fingerprint_finds_the_identity_it_belongs_to() {
        let mut one = GameIdentity::new("cloud-1", "one");
        one.merge_machine(machine("machine-a", &["v1:10:aa"]));
        let mut two = GameIdentity::new("cloud-2", "two");
        two.merge_machine(machine("machine-b", &["v1:20:bb"]));
        let all = vec![
            ("games/one".to_string(), one),
            ("games/two".to_string(), two),
        ];

        let hits = GameIdentity::find_by_fingerprint(&all, "v1:20:bb");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.cloud_id, "cloud-2");
        // 键要跟着回来：rclone 那边就是"包放哪个目录"。
        assert_eq!(hits[0].0, "games/two");

        // 认不出来就是 0 个 —— "找不到就新建"，绝不硬凑一个。
        assert!(GameIdentity::find_by_fingerprint(&all, "v1:99:zz").is_empty());
        // 两张卡都报同一个指纹（复制过存档目录、或者建了两条档案）：**要问**。
        let mut three = GameIdentity::new("cloud-3", "three");
        three.merge_machine(machine("machine-c", &["v1:20:bb"]));
        let ambiguous = vec![
            ("games/two".to_string(), all[1].1.clone()),
            ("games/three".to_string(), three),
        ];
        assert_eq!(
            GameIdentity::find_by_fingerprint(&ambiguous, "v1:20:bb").len(),
            2
        );
    }

    #[test]
    fn an_identity_card_round_trips_and_tolerates_missing_optional_fields() {
        let mut identity = GameIdentity::new("8f2c", "示例游戏");
        identity.merge_machine(machine("1a2b", &["v1:184320000:9f3c"]));
        let text = serde_json::to_string_pretty(&identity).unwrap();
        assert_eq!(
            serde_json::from_str::<GameIdentity>(&text).unwrap(),
            identity
        );
        // 绝不放绝对路径与内容：整张卡里只该有身份、机器名、指纹、位置 key。
        assert!(!text.contains("/home/"), "{text}");
        assert!(text.contains("rel-savedata"), "{text}");

        // 手写的、或者将来少字段的卡也要读得进来（缺的按空处理，不猜）。
        let sparse = r#"{"format":1,"cloud_id":"c1"}"#;
        let parsed: GameIdentity = serde_json::from_str(sparse).unwrap();
        assert_eq!(parsed.machines.len(), 0);
        assert_eq!(parsed.cloud_id, "c1");
    }
}

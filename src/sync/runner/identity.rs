//! 身份的解析：上传之前把"这一版是谁的"定下来。
//!
//! 规则（用户 2026-09-21 拍板）：
//!   1. **本机已经认领过** ⇒ 就用它（粘住，指纹只当提议）；
//!   2. 否则**按 exe 指纹在云端找**同一款：恰好命中一个就认它（这就是"默认走指纹
//!      自动绑定"）；
//!   3. 找不到、命中多个、或者压根没有指纹 ⇒ **新建一个身份**。宁可新建，也不猜 ——
//!      猜错的代价是把别人的存档铺进本机这一款。
//!
//! 命中多个的那种（复制过存档目录、建了两条档案）留给配对界面（下一步）去问用户；
//! 在那之前，新建一个身份不会损坏任何东西，只是云端多了一条待合并的身份。
//!
//! 读一次对 kopia 是一次 `restore`（§5.4），所以"按指纹找"这条路**只在第一次上传
//! 一款游戏时**走一次，找到就落盘粘住。

use super::Runner;
use crate::sync::SyncError;
use crate::sync::cloud::{self, GameIdentity, MachineIdentity};

/// 定下来的身份，外加"它是怎么定下来的"（日志与界面要说得出区别）。
#[derive(Debug, Clone)]
pub struct Resolved {
    pub identity: GameIdentity,
    /// **这一款在云端该用的键**：rclone 是 `games/<key>/` 的目录名，kopia 是 `game:`
    /// 标签的值（= `cloud_id`）。所有版本都往这个键下面放。
    pub key: String,
    /// `true` = 云端本来就有这一款（本机认领/指纹认出）；`false` = 新建了一个。
    pub known: bool,
}

impl Runner {
    /// 云端与这一款对应的身份卡；没有就是 `None`。
    pub async fn read_identity(
        &self,
        game_id: &str,
        cloud_id: &str,
    ) -> Result<Option<GameIdentity>, SyncError> {
        self.backend
            .read_identity(game_id, cloud_id, self.work_dir())
            .await
    }

    /// 云端所有身份卡（带上各自的键）。
    pub async fn read_identities(&self) -> Result<Vec<(String, GameIdentity)>, SyncError> {
        self.backend.read_identities(self.work_dir()).await
    }

    /// 把一张身份卡写回云端，返回它的键。
    pub async fn write_identity(
        &self,
        game_id: &str,
        identity: &GameIdentity,
    ) -> Result<String, SyncError> {
        self.backend
            .write_identity(game_id, identity, self.work_dir())
            .await
    }

    /// 上传前把身份定下来，并把"自己这台机器"的信息并进去（**只追加**）。
    ///
    /// 无论走哪条路，最后都会把合并后的卡写回云端 —— 这样"这台机器也见过这一款"
    /// 才会被别的机器看见。
    pub async fn resolve_identity(
        &self,
        game_id: &str,
        name: &str,
        local_cloud_id: Option<&str>,
        machine: MachineIdentity,
    ) -> Result<Resolved, SyncError> {
        // 1. 认领过就用它：**身份是粘住的**，指纹只当提议。
        if let Some(cloud_id) = local_cloud_id {
            let mut identity = match self.read_identity(game_id, cloud_id).await? {
                Some(identity) => identity,
                // 云端那份丢了（换了台机器、或者第一次就撞上）：按本机知道的补一份。
                None => GameIdentity::new(cloud_id, name),
            };
            identity.merge_machine(machine);
            let key = self.write_identity(game_id, &identity).await?;
            return Ok(Resolved {
                identity,
                key,
                known: true,
            });
        }

        // 2. 按指纹找同一款。
        if let Some(fingerprint) = machine.fingerprints.first().cloned() {
            let identities = self.read_identities().await?;
            let hits = GameIdentity::find_by_fingerprint(&identities, &fingerprint);
            match hits.as_slice() {
                [hit] => {
                    let mut identity = hit.1.clone();
                    identity.merge_machine(machine);
                    // 跟它走**同一个键**：版本放进同一个目录，两台机器才互相看得见。
                    let key = self.write_identity(&hit.0, &identity).await?;
                    tracing::info!(
                        "{game_id}: exe 指纹认出了云端身份 {}",
                        cloud::short_id(&identity.cloud_id, 8)
                    );
                    return Ok(Resolved {
                        identity,
                        key,
                        known: true,
                    });
                }
                [] => {}
                many => tracing::warn!(
                    "{game_id}: exe 指纹在云端命中了 {} 个身份，不猜，先新建一个（等配对）",
                    many.len()
                ),
            }
        }

        // 3. 新建。（一台机器一份的 `machine_id` 由调用方给，见 `daemon/sync_rpc`。）
        let cloud_id = uuid::Uuid::new_v4().to_string();
        let mut identity = GameIdentity::new(&cloud_id, name);
        identity.merge_machine(machine);
        let key = self.write_identity(game_id, &identity).await?;
        tracing::info!(
            "{game_id}: 云端身份新建为 {}",
            cloud::short_id(&cloud_id, 8)
        );
        Ok(Resolved {
            identity,
            key,
            known: false,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::sync::cloud::IDENTITY_FILE;
    use crate::sync::runner::testing::FakeRclone;

    fn machine(id: &str, fingerprint: &str) -> MachineIdentity {
        MachineIdentity {
            machine_id: id.to_string(),
            label: format!("host-{id}"),
            fingerprints: vec![fingerprint.to_string()],
            locations: vec!["rel-savedata".to_string()],
            parents: Vec::new(),
            exe_paths: vec![format!("/games/{id}/game.exe")],
        }
    }

    /// 桶里某个目录的身份卡（"云端真的有这张卡吗"）。
    fn card(fake: &FakeRclone, dir: &str) -> GameIdentity {
        let path = fake.bucket_path(&format!("kotori:bkt/prefix/games/{dir}/{IDENTITY_FILE}"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} 没读着: {e}", path.display()));
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn a_first_upload_mints_one_identity_and_writes_the_card() {
        let fake = FakeRclone::new("identity-new");
        let runner = fake.runner(0);
        let resolved = runner
            .resolve_identity("demo", "Demo", None, machine("machine-a", "v1:10:aa"))
            .await
            .unwrap();

        assert!(!resolved.known, "云端本来没有，这是新建的");
        assert_eq!(resolved.identity.cloud_id.len(), 36, "身份是个 uuid");
        assert_eq!(resolved.key, "demo", "落点就是本机的游戏 id");

        // 卡写进桶里了（与包并排），而且只有自己那台机器。
        let written = card(&fake, "demo");
        assert_eq!(written, resolved.identity);
        assert_eq!(written.machines.len(), 1);
        assert_eq!(
            written.machines[0].fingerprints,
            vec!["v1:10:aa".to_string()]
        );

        // 身份卡不是包：版本列表里一版都不该多出来。
        assert!(runner.packages("demo").await.unwrap().is_empty());

        std::fs::remove_dir_all(&fake.dir).ok();
    }

    #[tokio::test]
    async fn a_second_machine_adopts_the_identity_its_fingerprint_points_at() {
        let fake = FakeRclone::new("identity-adopt");
        // A 机：游戏叫 Other Name，目录就是它自己的 id。
        let first = fake
            .runner(0)
            .resolve_identity(
                "other-name",
                "Other Name",
                None,
                machine("machine-a", "v1:10:aa"),
            )
            .await
            .unwrap();

        // B 机：**另一个游戏 id、另一个名字**，但 exe 是同一个（指纹一样）。
        let runner = fake.runner(0);
        let adopted = runner
            .resolve_identity("demo", "Demo", None, machine("machine-b", "v1:10:aa"))
            .await
            .unwrap();

        assert!(adopted.known, "指纹命中就该认它，而不是新建一个");
        assert_eq!(adopted.identity.cloud_id, first.identity.cloud_id);
        assert_eq!(adopted.identity.machines.len(), 2, "两台机器各占一条");
        // ⚠ **落点要跟着身份走**（A 那个目录）：不然两台机器各写各的目录，
        // 版本永远互相看不见 —— 这正是 `cloud_dir` 存在的理由。
        assert_eq!(adopted.key, "other-name");

        // 写到 A 那个目录里去了，两边是同一张卡。
        let merged = card(&fake, "other-name");
        assert_eq!(merged.cloud_id, first.identity.cloud_id);
        assert_eq!(merged.machines.len(), 2);
        assert!(
            !fake
                .bucket_path("kotori:bkt/prefix/games/demo")
                .join(IDENTITY_FILE)
                .exists(),
            "不该在 demo 目录下另立一张卡"
        );

        std::fs::remove_dir_all(&fake.dir).ok();
    }

    #[tokio::test]
    async fn a_claimed_identity_is_sticky_even_when_the_fingerprint_changes() {
        let fake = FakeRclone::new("identity-sticky");
        let runner = fake.runner(0);
        let first = runner
            .resolve_identity("demo", "Demo", None, machine("machine-a", "v1:10:aa"))
            .await
            .unwrap();

        // 同一个 exe 换了（重装、打补丁）：指纹不一样了，身份**不许**自己变 ——
        // 那会把"这还算不算同一款"替用户改了答案。
        let again = runner
            .resolve_identity(
                "demo",
                "Demo",
                Some(&first.identity.cloud_id),
                machine("machine-a", "v1:20:bb"),
            )
            .await
            .unwrap();
        assert!(again.known);
        assert_eq!(again.identity.cloud_id, first.identity.cloud_id);
        assert_eq!(
            again.identity.machines[0].fingerprints,
            vec!["v1:10:aa".to_string(), "v1:20:bb".to_string()],
            "新的指纹只追加"
        );

        std::fs::remove_dir_all(&fake.dir).ok();
    }

    #[tokio::test]
    async fn a_directory_taken_by_another_identity_gets_a_suffix_not_a_second_helping() {
        let fake = FakeRclone::new("identity-occupied");
        // 别的身份先占了 `demo` 这个目录（§5.7）。
        let mut stranger = GameIdentity::new("stranger-cloud-id", "Someone Else");
        stranger.merge_machine(machine("machine-x", "v1:99:zz"));
        let runner = fake.runner(0);
        let key = runner.write_identity("demo", &stranger).await.unwrap();
        assert_eq!(key, "demo");

        let mine = runner
            .resolve_identity("demo", "Demo", None, machine("machine-a", "v1:10:aa"))
            .await
            .unwrap();
        assert_eq!(
            mine.key,
            format!(
                "demo-{}",
                crate::sync::cloud::short_id(&mine.identity.cloud_id, 6)
            ),
            "被占了就加身份后缀 —— 不是 `-2`（那看着像同款第二份）"
        );
        assert_eq!(card(&fake, &mine.key).cloud_id, mine.identity.cloud_id);
        assert_eq!(card(&fake, "demo").cloud_id, "stranger-cloud-id");

        std::fs::remove_dir_all(&fake.dir).ok();
    }
}

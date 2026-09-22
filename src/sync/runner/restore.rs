//! 恢复与保留窗口：把云端某一版铺回本机（`restore`），以及按滑动窗口删掉云上的
//! 旧版本（`prune`）。
//!
//! 与 `pull.rs` 分开：这里的每一步都可能覆盖本机数据，所以语义必须是"用户说了
//! 算"（[`Merge::Replace`]）。**恢复前不再把本机推上云**——那一手正是从前"恢复
//! 到最新"变成空操作的根因（自保快照先把本机推进了 current/，随后的恢复自然是
//! 无变化）。恢复的可撤销性由"上一版还在"保证：每一版都是完整的，回退到上一版
//! 就是撤销。

use super::staging::Staging;
use super::{COMMAND_TIMEOUT, GameOutcome, LocationOutcome, Runner};
use crate::sync::archive::{self, Merge};
use crate::sync::cloud::identity_match;
use crate::sync::{SaveTarget, SyncError, is_snapshot, prune_plan};

impl Runner {
    /// Put a game's saves back.
    ///
    /// `version = None` restores the newest package. A named version restores
    /// exactly that package: it holds *every* file of *every* location as it was
    /// then, so rolling back is laying it down, not un-picking a diff.
    ///
    /// ⚠ 手动恢复也过**防错配闸**：用户确实说了"就铺这一版"，但他没说"铺错一款
    /// 也行"。身份对不上（或者缺一头）时一个文件都不铺，理由与 [`Runner::pull`]
    /// 同一套 —— 覆盖本机存档是这条路上唯一不可逆的事。
    pub async fn restore(
        &self,
        game_id: &str,
        name: &str,
        targets: &[SaveTarget],
        local_cloud_id: Option<&str>,
        version: Option<&str>,
    ) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }
        if let Some(version) = version
            && !is_snapshot(version)
        {
            return GameOutcome::failed(
                game_id,
                name,
                format!("不是合法的版本名: {version}（形如 20260911T101500Z）"),
            );
        }

        let stamp = match version {
            Some(version) => version.to_string(),
            None => match self.latest_package(game_id).await {
                Ok(Some(stamp)) => stamp,
                Ok(None) => {
                    return GameOutcome::from_locations(
                        game_id,
                        name,
                        targets
                            .iter()
                            .map(|target| {
                                LocationOutcome::new(target, "skipped", "云端还没有这个游戏的存档")
                            })
                            .collect(),
                    );
                }
                Err(error) => return GameOutcome::failed(game_id, name, error.to_string()),
            },
        };

        let staging = match Staging::new(self.work_dir()) {
            Ok(staging) => staging,
            Err(error) => return GameOutcome::failed(game_id, name, error),
        };
        let manifest = match self
            .fetch_version(game_id, &stamp, &staging.unpacked(), COMMAND_TIMEOUT)
            .await
        {
            Ok(manifest) => manifest,
            Err(error) => {
                return GameOutcome::failed(game_id, name, format!("取不回版本 {stamp}: {error}"));
            }
        };
        // 闸门在**铺文件之前**：清单已经读出来了，本机还一个字节都没动。
        if let Some(refusal) = identity_match(local_cloud_id, manifest.identity.as_ref()).refusal()
        {
            return GameOutcome::failed(
                game_id,
                name,
                format!("{refusal}（云端那一版是 {stamp}）"),
            );
        }
        // 用户点了"恢复"：以云端为准，本机更新的也盖掉。
        let plan = match archive::plan(&manifest, targets, Merge::Replace) {
            Ok(plan) => plan,
            Err(error) => return GameOutcome::failed(game_id, name, error),
        };
        if let Err(error) = staging.lay_down(targets, &plan) {
            return GameOutcome::failed(game_id, name, format!("写入本机存档失败: {error}"));
        }

        let outcomes = targets
            .iter()
            .map(|target| {
                if !manifest.has_location(&target.key) {
                    return LocationOutcome::new(target, "skipped", "云端还没有这个位置的存档");
                }
                let mut detail = format!("已恢复到 {stamp}");
                // 回退之后本机可能还剩这一版里没有的文件，游戏照旧可能读到它们。
                // **只报不删**：删本地数据永远是用户点头才做的事。
                let extras: Vec<&String> = plan
                    .extras
                    .iter()
                    .filter(|name| name.starts_with(&format!("{}/", target.key)))
                    .collect();
                if !extras.is_empty() {
                    detail.push_str(&format!(
                        "；本机另有 {} 个文件不在这一版里（保留未动）：{}",
                        extras.len(),
                        summarize(&extras)
                    ));
                }
                LocationOutcome::new(target, "restored", detail)
            })
            .collect();

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Delete the packages that fall outside the retention window.
    ///
    /// Never touches local files, and never touches a cloud object that does not
    /// look like one of our own packages.
    pub async fn prune(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let stamps = self.packages(game_id).await?;
        let doomed = prune_plan(&stamps, self.settings.keep_versions);
        for stamp in &doomed {
            self.remove_version(game_id, stamp).await?;
            tracing::info!("{game_id}: 已删除旧版本 {stamp}");
        }
        Ok(doomed)
    }
}

/// 列出最靠前的几个名字，其余用省略号收尾——一行提示不该被一屏文件名撑爆。
fn summarize(names: &[&String]) -> String {
    const SHOWN: usize = 3;
    let head: Vec<&str> = names.iter().take(SHOWN).map(|name| name.as_str()).collect();
    if names.len() > SHOWN {
        format!("{} 等", head.join("、"))
    } else {
        head.join("、")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::sync::archive;
    use crate::sync::cloud::PackIdentity;
    use crate::sync::runner::testing::{FakeRclone, target};

    /// 本机这一款的云端身份。云端那些包由 [`publish`] 带着它上传。
    const CLOUD_ID: &str = "cloud-demo";

    fn identity() -> PackIdentity {
        PackIdentity {
            cloud_id: CLOUD_ID.to_string(),
            machine_id: Some("machine-a".to_string()),
            fingerprint: None,
            locations: vec!["rel-savedata".to_string()],
        }
    }

    fn publish(fake: &FakeRclone, saves: &std::path::Path, stamp: &str, body: &str) {
        std::fs::create_dir_all(saves).unwrap();
        std::fs::write(saves.join("save.sav"), body).unwrap();
        let target = target(saves, "savedata", "rel-savedata");
        let zip = fake.dir.join(format!("publish-{stamp}.zip"));
        archive::pack(&zip, &[target], chrono::Utc::now(), Some(&identity())).unwrap();
        fake.put_package("demo", stamp, &zip);
    }

    #[tokio::test]
    async fn restoring_lays_the_package_down_over_whatever_is_local() {
        let fake = FakeRclone::new("restore");
        let cloud = fake.dir.join("cloud");
        publish(&fake, &cloud, "20260901T000000Z", "cloud one");
        publish(&fake, &cloud, "20260902T000000Z", "cloud two");

        // 本机是新的一版，而且已经被改坏了。
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), "corrupted").unwrap();

        let outcome = fake
            .runner(0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
                Some("20260901T000000Z"),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "restored");
        assert!(outcome.locations[0].detail.contains("20260901T000000Z"));
        assert_eq!(
            std::fs::read_to_string(saves.join("save.sav")).unwrap(),
            "cloud one",
            "an explicit restore is meant to win, even over a newer local file"
        );

        // 恢复不再"先把本机推上云"：那次自保快照正是"恢复到最新"变成空操作的根因。
        let calls = fake.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("copy ")),
            "no safety snapshot: {calls:?}"
        );
        assert!(calls.iter().any(|call| call.starts_with("copyto ")));
    }

    #[tokio::test]
    async fn restoring_without_a_version_takes_the_newest_package() {
        let fake = FakeRclone::new("restore-latest");
        let cloud = fake.dir.join("cloud");
        publish(&fake, &cloud, "20260901T000000Z", "older");
        publish(&fake, &cloud, "20260902T000000Z", "newer");

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
                None,
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert!(outcome.locations[0].detail.contains("20260902T000000Z"));
        assert_eq!(
            std::fs::read_to_string(saves.join("save.sav")).unwrap(),
            "newer"
        );
    }

    #[tokio::test]
    async fn extra_local_files_are_listed_and_never_deleted() {
        let fake = FakeRclone::new("restore-extras");
        let cloud = fake.dir.join("cloud");
        publish(&fake, &cloud, "20260901T000000Z", "cloud");

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), "local").unwrap();
        std::fs::write(saves.join("only-local.sav"), "keep me").unwrap();

        let outcome = fake
            .runner(0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
                None,
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        let detail = &outcome.locations[0].detail;
        assert!(detail.contains("保留未动"), "{detail}");
        assert!(detail.contains("only-local.sav"), "{detail}");
        assert_eq!(
            std::fs::read_to_string(saves.join("only-local.sav")).unwrap(),
            "keep me",
            "removing local data is the user's call, never ours"
        );
    }

    #[tokio::test]
    async fn a_bogus_version_name_is_refused_before_anything_is_touched() {
        let fake = FakeRclone::new("restore-bogus");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
                Some("../../../etc"),
            )
            .await;

        assert!(!outcome.ok);
        assert!(outcome.error.unwrap().contains("不是合法的版本名"));
        assert!(
            fake.calls().is_empty(),
            "nothing may run: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn retention_only_ever_deletes_our_own_old_packages() {
        let fake = FakeRclone::new("prune");
        for stamp in ["20260903T000000Z", "20260901T000000Z", "20260902T000000Z"] {
            fake.put(&format!("kotori:bkt/prefix/games/demo/{stamp}.zip"), "old");
        }
        fake.put("kotori:bkt/prefix/games/demo/notes.txt", "not ours");

        let removed = fake.runner(2).prune("demo").await.unwrap();
        assert_eq!(removed, vec!["20260901T000000Z".to_string()]);
        assert_eq!(
            fake.package_names("demo"),
            vec![
                "20260902T000000Z".to_string(),
                "20260903T000000Z".to_string()
            ]
        );
        assert!(
            fake.remote_exists("kotori:bkt/prefix/games/demo/notes.txt"),
            "pruning must never touch an object it does not recognise"
        );
    }

    #[tokio::test]
    async fn retention_keeps_everything_unless_the_user_asked_otherwise() {
        let fake = FakeRclone::new("prune-off");
        for stamp in ["20260901T000000Z", "20260902T000000Z"] {
            fake.put(&format!("kotori:bkt/prefix/games/demo/{stamp}.zip"), "old");
        }

        assert!(fake.runner(0).prune("demo").await.unwrap().is_empty());
        assert!(fake.calls_matching("deletefile").is_empty());
        // 保留窗口比版本数大：什么都不该删。
        assert!(fake.runner(5).prune("demo").await.unwrap().is_empty());
        assert!(fake.calls_matching("deletefile").is_empty());
    }

    /// 手动恢复也过**防错配闸**：身份对不上时一个文件都不铺。
    ///
    /// 用户说了"铺这一版"，但他没说"铺错一款也行" —— 覆盖本机存档是这条路上唯一
    /// 不可逆的事，所以手动这条路与启动前的自动取回同一条规矩。
    #[tokio::test]
    async fn restoring_another_identity_is_refused_and_touches_nothing() {
        let fake = FakeRclone::new("restore-other-identity");
        let cloud = fake.dir.join("cloud");
        publish(&fake, &cloud, "20260901T000000Z", "someone else's save");

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let local = saves.join("save.sav");
        std::fs::write(&local, "my own progress").unwrap();

        let outcome = fake
            .runner(0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("another-identity"),
                None,
            )
            .await;

        assert!(!outcome.ok, "{outcome:?}");
        let error = outcome.error.unwrap();
        assert!(error.contains("另一个身份"), "{error}");
        assert!(error.contains("本机存档一个都没动"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&local).unwrap(),
            "my own progress",
            "闸门必须在铺文件之前拦下来"
        );
    }
}

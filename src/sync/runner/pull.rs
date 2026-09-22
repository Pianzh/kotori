//! 启动前取回：把云端最新那一版里**比本机新**的文件取回来（`pull`）。
//!
//! ADR-012 的落点。从前这条不变量是 `rclone --update` 保证的——它逐文件比较，
//! 只覆盖更新的那些。改成一版一包之后，rclone 不再看文件，所以这条保证搬到了
//! 我们自己手里：下载最新包、按清单逐文件比较、只铺 `plan.take`。
//!
//! 报价必须诚实：超时或失败要报成"没取完"，**绝不能报成"云端没有存档"**——
//! 后者听起来像是一切正常，用户会以为自己的进度已经在云上了。

use std::collections::HashMap;

use super::staging::Staging;
use super::{GameOutcome, LocationOutcome, PULL_TIMEOUT, Runner};
use crate::sync::archive::{self, Merge};
use crate::sync::cloud::identity_match;

impl Runner {
    /// Fetch anything that is *newer* in the cloud, keeping newer local files.
    ///
    /// Used before a launch: a slow network or a broken package must never turn
    /// into "the game did not start", so every failure here is reported, not
    /// raised, and the caller launches anyway.
    ///
    /// ⚠ `local_cloud_id` 是**防错配闸**：云端那一版的身份与本机这一款不一致
    /// （或者两边缺一头）时，这一版**一个文件都不会铺** —— 见 [`identity_match`]。
    /// 这条是踩过的坑的反面：两台机器给两款不同游戏起的名字撞在一起时，静默铺过去
    /// 就是**静默损坏存档**。
    pub async fn pull(
        &self,
        game_id: &str,
        name: &str,
        targets: &[crate::sync::SaveTarget],
        local_cloud_id: Option<&str>,
    ) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let stamp = match self.latest_package(game_id).await {
            Ok(Some(stamp)) => stamp,
            // Nothing has ever been uploaded: not an error, just nothing to do.
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
        };

        let staging = match Staging::new(self.work_dir()) {
            Ok(staging) => staging,
            Err(error) => return GameOutcome::failed(game_id, name, error),
        };
        let manifest = match self
            .fetch_version(game_id, &stamp, &staging.unpacked(), PULL_TIMEOUT)
            .await
        {
            Ok(manifest) => manifest,
            Err(error) => {
                return GameOutcome::failed(
                    game_id,
                    name,
                    format!("没能取回云端存档 {stamp}（这一局照常启动）: {error}"),
                );
            }
        };
        // 闸门在**铺文件之前**：清单已经读出来了，本机还一个字节都没动。
        if let Some(refusal) = identity_match(local_cloud_id, manifest.identity.as_ref()).refusal()
        {
            return GameOutcome::failed(
                game_id,
                name,
                format!("{refusal}（云端最新那一版是 {stamp}，这一局照常启动）"),
            );
        }
        let plan = match archive::plan(&manifest, targets, Merge::Newer) {
            Ok(plan) => plan,
            Err(error) => {
                return GameOutcome::failed(game_id, name, format!("合并判定失败: {error}"));
            }
        };
        if let Err(error) = staging.lay_down(targets, &plan) {
            return GameOutcome::failed(game_id, name, format!("写入本机存档失败: {error}"));
        }

        let mut taken: HashMap<&str, usize> = HashMap::new();
        for entry in &plan.take {
            *taken.entry(entry.key.as_str()).or_default() += 1;
        }
        let mut kept: HashMap<&str, usize> = HashMap::new();
        for entry in &plan.kept {
            *kept.entry(entry.key.as_str()).or_default() += 1;
        }

        let outcomes = targets
            .iter()
            .map(|target| {
                if !manifest.has_location(&target.key) {
                    return LocationOutcome::new(target, "skipped", "云端还没有这个位置的存档");
                }
                let taken = taken.get(target.key.as_str()).copied().unwrap_or(0);
                let kept = kept.get(target.key.as_str()).copied().unwrap_or(0);
                if taken > 0 {
                    LocationOutcome::new(
                        target,
                        "pulled",
                        format!("已取回云端较新的 {taken} 个文件（{stamp}）"),
                    )
                } else if kept > 0 {
                    // 这一条是 ADR-012 说出口的地方：本机更新就不动。
                    LocationOutcome::new(
                        target,
                        "kept",
                        format!("本机的 {kept} 个文件更新，保持不动"),
                    )
                } else {
                    LocationOutcome::new(target, "pulled", format!("云端 {stamp} 里这个位置是空的"))
                }
            })
            .collect();

        GameOutcome::from_locations(game_id, name, outcomes)
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

    /// 把一个包放到云端：先在本地打一个，再让假 rclone 搬过去。
    ///
    /// `mtime_ms` 显式给定，不靠"文件刚写完"——两次写入之间只差几毫秒，而 ms
    /// 精度下它们可能落在同一刻度上，那这条测试就会时绿时红。
    fn publish(fake: &FakeRclone, saves: &std::path::Path, stamp: &str, body: &str, mtime_ms: i64) {
        publish_as(Some(&identity()), fake, saves, stamp, body, mtime_ms);
    }

    /// 同上，但可以指定包里的身份（`None` = 更早的 kotori 传的那种没有身份的包）。
    fn publish_as(
        identity: Option<&PackIdentity>,
        fake: &FakeRclone,
        saves: &std::path::Path,
        stamp: &str,
        body: &str,
        mtime_ms: i64,
    ) {
        std::fs::create_dir_all(saves).unwrap();
        let path = saves.join("save.sav");
        std::fs::write(&path, body).unwrap();
        set_mtime_ms(&path, mtime_ms);
        let target = target(saves, "savedata", "rel-savedata");
        let zip = fake.dir.join("publish.zip");
        archive::pack(&zip, &[target], chrono::Utc::now(), identity).unwrap();
        fake.put_package("demo", stamp, &zip);
    }

    fn set_mtime_ms(path: &std::path::Path, ms: i64) {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[tokio::test]
    async fn a_pull_takes_the_newest_package_and_leaves_newer_local_files_alone() {
        let fake = FakeRclone::new("pull");
        let cloud = fake.dir.join("cloud-saves");
        // 云端那一版很旧（1970 年的第 1 秒），本机这一份是刚写的。
        publish(&fake, &cloud, "20260901T000000Z", "from the cloud", 1_000);

        // 本机版本更新：上一次上传失败了，用户的进度只在本机。
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), "local and newer").unwrap();
        let target = target(&saves, "savedata", "rel-savedata");

        let outcome = fake
            .runner(0)
            .pull("demo", "Demo", &[target], Some(CLOUD_ID))
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "kept");
        assert!(
            outcome.locations[0].detail.contains("保持不动"),
            "{:?}",
            outcome.locations[0]
        );
        assert_eq!(
            std::fs::read_to_string(saves.join("save.sav")).unwrap(),
            "local and newer",
            "a pull must never eat the progress the user just made"
        );
    }

    #[tokio::test]
    async fn a_pull_brings_back_files_the_cloud_has_newer_versions_of() {
        let fake = FakeRclone::new("pull-newer");
        let cloud = fake.dir.join("cloud-saves");
        publish(&fake, &cloud, "20260901T000000Z", "from the cloud", 2_000);

        // 本机是旧的（时间戳被推回更早）。
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let local = saves.join("save.sav");
        std::fs::write(&local, "old local").unwrap();
        set_mtime_ms(&local, 1_000);

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "pulled");
        assert_eq!(
            std::fs::read_to_string(saves.join("save.sav")).unwrap(),
            "from the cloud"
        );
    }

    #[tokio::test]
    async fn a_game_the_cloud_has_never_seen_is_not_an_error() {
        let fake = FakeRclone::new("pull-empty");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(outcome.locations[0].detail.contains("云端还没有"));
        assert_eq!(fake.calls().len(), 1, "only the listing happened");
    }

    #[tokio::test]
    async fn a_package_that_cannot_be_fetched_says_so_instead_of_pretending_there_is_none() {
        let fake = FakeRclone::new("pull-broken");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.put(
            "kotori:bkt/prefix/games/demo/20260901T000000Z.zip",
            "garbage",
        );
        // 假 rclone 把对象原样搬下来，所以这里下来的是一个坏包。
        std::fs::write(saves.join("save.sav"), "local").unwrap();

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
            )
            .await;

        assert!(!outcome.ok);
        let error = outcome.error.unwrap();
        assert!(error.contains("读不出来"), "{error}");
        assert!(
            !error.contains("云端还没有"),
            "a broken package is not 'nothing to do': {error}"
        );
        assert_eq!(
            std::fs::read_to_string(saves.join("save.sav")).unwrap(),
            "local"
        );
    }

    #[tokio::test]
    async fn a_location_the_package_does_not_cover_is_reported_separately() {
        let fake = FakeRclone::new("pull-missing-location");
        let cloud = fake.dir.join("cloud-saves");
        publish(&fake, &cloud, "20260901T000000Z", "cloud", 1_000);

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[
                    target(&saves, "savedata", "rel-savedata"),
                    // 这一台机器上还有另一个位置，云端这一版里没有它。
                    target(&saves, "extra", "rel-extra"),
                ],
                Some(CLOUD_ID),
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[1].action, "skipped");
        assert!(outcome.locations[1].detail.contains("这个位置"));
    }

    /// 云端那一版是**别的一款**（身份对不上）：一个文件都不许铺。
    ///
    /// 这条盯的是整件事里唯一不可逆的错误：把别人的存档铺进本机这一款，
    /// 用户下一次上传再把它推回云端 —— **静默损坏存档**。
    #[tokio::test]
    async fn a_pull_from_another_identity_touches_nothing() {
        let fake = FakeRclone::new("pull-other-identity");
        let cloud = fake.dir.join("cloud");
        publish(
            &fake,
            &cloud,
            "20260901T000000Z",
            "someone else's save",
            2_000,
        );

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let local = saves.join("save.sav");
        std::fs::write(&local, "my own progress").unwrap();
        set_mtime_ms(&local, 1_000);

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("another-identity"),
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

    /// 本机这一款还没认领过身份：不猜，直接跳过（取回那条路**不认领**身份）。
    #[tokio::test]
    async fn a_pull_before_this_machine_claimed_an_identity_is_skipped() {
        let fake = FakeRclone::new("pull-unpaired");
        let cloud = fake.dir.join("cloud");
        publish(&fake, &cloud, "20260901T000000Z", "from the cloud", 2_000);

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                None,
            )
            .await;

        assert!(!outcome.ok, "{outcome:?}");
        let error = outcome.error.unwrap();
        assert!(error.contains("还没有云端身份"), "{error}");
        assert!(
            !saves.join("save.sav").exists(),
            "跳过就是跳过：一个文件都不该落下来"
        );
    }

    /// 云端那一版是更早的 kotori 传的（清单里没有身份段）：同样不猜。
    #[tokio::test]
    async fn a_pull_refuses_a_package_without_any_identity() {
        let fake = FakeRclone::new("pull-no-identity");
        let cloud = fake.dir.join("cloud");
        publish_as(
            None,
            &fake,
            &cloud,
            "20260901T000000Z",
            "old package",
            2_000,
        );

        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(CLOUD_ID),
            )
            .await;

        assert!(!outcome.ok, "{outcome:?}");
        let error = outcome.error.unwrap();
        assert!(error.contains("没有身份信息"), "{error}");
        assert!(!saves.join("save.sav").exists());
    }
}

//! 存档的上行：把本机这一版打成一个包推上云端（`upload`）。
//!
//! 与 `pull.rs` 分开：上传是"我说了算"——包打的是本机现状，旧包一个都不动
//! （失败也不动），所以上传失败最多是这一版没上去，回退能力毫发无损。

use super::staging::Staging;
use super::{COMMAND_TIMEOUT, GameOutcome, LocationOutcome, Runner};
use crate::sync::cloud::PackIdentity;

impl Runner {
    /// Push every location of a game into the cloud as one package.
    ///
    /// 全量上传，不做"内容没变就跳过"：想省空间的人用 kopia（用户 2026-09-15
    /// 明确）。换来的是"一个包 = 一个时间点的完整存档"，恢复因此不需要拼差量。
    ///
    /// `identity` 写进包清单：**取回之前比的就是它**（见 `super::pull`）。
    pub async fn upload(
        &self,
        game_id: &str,
        name: &str,
        cloud_key: &str,
        targets: &[crate::sync::SaveTarget],
        identity: Option<&PackIdentity>,
    ) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let staging = match Staging::new(self.work_dir()) {
            Ok(staging) => staging,
            Err(error) => return GameOutcome::failed(game_id, name, error),
        };
        let stamp = crate::sync::version_stamp(chrono::Utc::now());

        let send = self
            .send_version(
                cloud_key,
                &stamp,
                targets,
                identity,
                staging.root(),
                COMMAND_TIMEOUT,
            )
            .await;

        // 本机一个存档目录都没有：引擎已经不上传了（两个引擎共用这条判据），
        // 这里只把"没东西可传"如实说出来。
        if let Ok(report) = &send
            && report.locations.is_empty()
        {
            return GameOutcome::from_locations(
                game_id,
                name,
                targets
                    .iter()
                    .map(|target| {
                        LocationOutcome::new(target, "skipped", "本地没有这个目录，没什么可上传的")
                    })
                    .collect(),
            );
        }

        if let Ok(report) = &send {
            tracing::info!(
                "{game_id}: 打包完成 {stamp}（{} 个文件，跳过 {} 个被排除的）",
                report.entries.len(),
                report.excluded
            );
        }

        let outcomes = targets
            .iter()
            .map(|target| match &send {
                Err(error) => LocationOutcome::new(target, "failed", error.to_string()),
                Ok(report) if report.missing.contains(&target.key) => {
                    LocationOutcome::new(target, "skipped", "本地没有这个目录，没什么可上传的")
                }
                Ok(_) => LocationOutcome::new(target, "uploaded", format!("已上传为 {stamp}")),
            })
            .collect::<Vec<_>>();

        // Retention is a separate, best-effort step: failing to tidy up must
        // never turn a successful upload into a failure.
        if send.is_ok()
            && self.settings.keep_versions > 0
            && let Err(error) = self.prune(cloud_key).await
        {
            tracing::warn!("{game_id}: 清理旧版本失败: {error}");
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::sync::archive;
    use crate::sync::cloud::PackIdentity;
    use crate::sync::runner::testing::{FakeRclone, target};

    /// 一个"这一版是谁传的"。取回那边比的就是它（见 `super::super::pull`）。
    fn identity() -> PackIdentity {
        PackIdentity {
            cloud_id: "cloud-demo".to_string(),
            machine_id: Some("machine-a".to_string()),
            fingerprint: None,
            locations: vec!["rel-savedata".to_string()],
        }
    }

    #[tokio::test]
    async fn upload_sends_the_whole_game_as_one_package() {
        let fake = FakeRclone::new("upload");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(saves.join("nested")).unwrap();
        std::fs::write(saves.join("save01.sav"), "one").unwrap();
        std::fs::write(saves.join("nested/save02.sav"), "two").unwrap();

        let mut target = target(&saves, "savedata", "rel-savedata");
        target.exclude = vec!["*.log".to_string()];
        std::fs::write(saves.join("debug.log"), "noise").unwrap();

        let outcome = fake
            .runner(0)
            .upload(
                "demo",
                "Demo",
                "demo",
                std::slice::from_ref(&target),
                Some(&identity()),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "uploaded");

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "one package, one transfer: {calls:?}");
        assert!(calls[0].starts_with("copyto "), "{calls:?}");
        assert!(
            calls[0].contains("kotori:bkt/prefix/games/demo/"),
            "{calls:?}"
        );
        assert!(
            !calls[0].contains("--backup-dir") && !calls[0].contains("--delete"),
            "a package upload cannot delete anything: {calls:?}"
        );

        // 云端真的多了一个包，而且里面是完整的、排除了 .log 的那一版。
        let packages = fake.package_names("demo");
        assert_eq!(packages.len(), 1, "{packages:?}");
        let unpacked = fake.dir.join("check");
        let manifest =
            archive::extract(&fake.package_path("demo", &packages[0]), &unpacked).unwrap();
        assert_eq!(manifest.entries.len(), 2);
        assert!(
            std::fs::read_to_string(unpacked.join("rel-savedata/save01.sav")).unwrap() == "one"
        );
        assert!(!unpacked.join("rel-savedata/debug.log").exists());
        // 身份真的进了包，而且读得回来 —— 取回那条路的闸门全靠它。
        assert_eq!(
            manifest.identity,
            Some(identity()),
            "包必须随身带着身份，否则取回时只能靠猜"
        );
    }

    #[tokio::test]
    async fn a_location_that_does_not_exist_locally_is_reported_not_ignored() {
        let fake = FakeRclone::new("missing");
        let absent = fake.dir.join("never-created");
        let outcome = fake
            .runner(0)
            .upload(
                "demo",
                "Demo",
                "demo",
                &[target(&absent, "savedata", "rel-savedata")],
                Some(&identity()),
            )
            .await;

        assert!(
            outcome.ok,
            "nothing to upload is not a failure: {outcome:?}"
        );
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(fake.calls().is_empty(), "no rclone call was needed");
        assert!(
            fake.package_names("demo").is_empty(),
            "an empty package must not be uploaded"
        );
    }

    #[tokio::test]
    async fn one_missing_location_does_not_hold_back_the_others() {
        let fake = FakeRclone::new("partial-missing");
        let present = fake.dir.join("here");
        std::fs::create_dir_all(&present).unwrap();
        std::fs::write(present.join("save.sav"), "one").unwrap();

        let outcome = fake
            .runner(0)
            .upload(
                "demo",
                "Demo",
                "demo",
                &[
                    target(&present, "here", "rel-here"),
                    target(&fake.dir.join("elsewhere"), "there", "rel-there"),
                ],
                Some(&identity()),
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "uploaded");
        assert_eq!(outcome.locations[1].action, "skipped");

        let packages = fake.package_names("demo");
        let unpacked = fake.dir.join("check");
        let manifest =
            archive::extract(&fake.package_path("demo", &packages[0]), &unpacked).unwrap();
        assert!(manifest.has_location("rel-here"));
        assert!(!manifest.has_location("rel-there"));
    }

    #[tokio::test]
    async fn a_failed_transfer_is_named_and_leaves_the_old_packages_alone() {
        let fake = FakeRclone::new("partial");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), "new").unwrap();
        fake.put(
            "kotori:bkt/prefix/games/demo/20260901T000000Z.zip",
            "old package",
        );

        fake.fail_on("copyto ");

        let outcome = fake
            .runner(0)
            .upload(
                "demo",
                "Demo",
                "demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(&identity()),
            )
            .await;

        assert!(!outcome.ok);
        assert_eq!(outcome.locations[0].action, "failed");
        assert!(outcome.error.unwrap().contains("copyto"));
        // 旧包还在：这一版没上去，但回退能力一点没少。
        assert_eq!(
            fake.package_names("demo"),
            vec!["20260901T000000Z".to_string()]
        );
    }

    #[tokio::test]
    async fn retention_only_ever_deletes_our_own_old_packages() {
        let fake = FakeRclone::new("prune-on-upload");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("save.sav"), "one").unwrap();
        for stamp in ["20260901T000000Z", "20260902T000000Z", "20260903T000000Z"] {
            fake.put(&format!("kotori:bkt/prefix/games/demo/{stamp}.zip"), "old");
        }
        fake.put("kotori:bkt/prefix/games/demo/notes.txt", "not ours");

        // 上传这一版之后一共四个包，保留两个：最老的两个该走。
        let outcome = fake
            .runner(2)
            .upload(
                "demo",
                "Demo",
                "demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some(&identity()),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");

        let left = fake.package_names("demo");
        assert_eq!(left.len(), 2, "{left:?}");
        assert_eq!(left[0], "20260903T000000Z", "{left:?}");
        assert!(
            !left
                .iter()
                .any(|name| name.starts_with("20260901") || name.starts_with("20260902")),
            "the two oldest packages are the ones that go: {left:?}"
        );
        assert!(
            fake.remote_exists("kotori:bkt/prefix/games/demo/notes.txt"),
            "pruning must never touch an object it does not recognise"
        );
    }
}

//! 存档的上行生命周期：把本机目录推上云端（`upload`），以及在启动游戏前把云端
//! 较新的文件取回来（`pull`）。
//!
//! 与 `restore.rs` 分开：这两个方向都只是"合并"——上传把被替换的文件挪进快照，
//! 拉取只拿更新的；恢复（以及它为自保先做的快照）和保留窗口的清理在那边。
//! 两边挂在同一个 [`Runner`] 上，靠子模块关系共用它的私有字段和私有传输底座。

use super::{COMMAND_TIMEOUT, GameOutcome, LocationOutcome, Runner};
use crate::sync::{
    CURRENT_DIR, Merge, SaveTarget, copy_args, game_remote, version_stamp, versions_remote,
};

impl Runner {
    /// Push every location of a game into the cloud.
    pub async fn upload(&self, game_id: &str, name: &str, targets: &[SaveTarget]) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let stamp = version_stamp(chrono::Utc::now());
        let mut outcomes = Vec::with_capacity(targets.len());

        for target in targets {
            if !target.local.is_dir() {
                outcomes.push(LocationOutcome::new(
                    target,
                    "skipped",
                    "本地没有这个目录，没什么可上传的",
                ));
                continue;
            }

            let destination = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            // Replaced files land under their own snapshot, per location, so two
            // locations in the same game can never overwrite each other there.
            let backup = format!(
                "{}/{stamp}/{}",
                versions_remote(&self.settings, game_id),
                target.key
            );

            let mut args = copy_args(
                &target.local.to_string_lossy(),
                &destination,
                Some(&backup),
                Merge::Replace,
            );
            super::push_excludes(&mut args, &target.exclude);

            match self.run(&args, COMMAND_TIMEOUT).await {
                Ok(_) => outcomes.push(LocationOutcome::new(target, "uploaded", "已上传")),
                Err(error) => {
                    outcomes.push(LocationOutcome::new(target, "failed", error.to_string()))
                }
            }
        }

        // Retention is a separate, best-effort step: failing to tidy up must
        // never turn a successful upload into a failure.
        if self.settings.keep_versions > 0
            && let Err(error) = self.prune(game_id).await
        {
            tracing::warn!("{}: 清理旧快照失败: {error}", game_id);
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Fetch anything that is *newer* in the cloud, keeping newer local files.
    ///
    /// Used before a launch. `Merge::Newer` means a local save that was never
    /// uploaded (because the last upload failed) survives this.
    pub async fn pull(&self, game_id: &str, name: &str, targets: &[SaveTarget]) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }

        let available = match self.current_keys(game_id).await {
            Ok(keys) => keys,
            // Nothing has ever been uploaded: not an error, just nothing to do.
            Err(error) => return GameOutcome::failed(game_id, name, error.to_string()),
        };

        let mut outcomes = Vec::with_capacity(targets.len());
        for target in targets {
            if !available.contains(&target.key) {
                outcomes.push(LocationOutcome::new(
                    target,
                    "skipped",
                    "云端还没有这个位置的存档",
                ));
                continue;
            }

            let source = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            let mut args = copy_args(&source, &target.local.to_string_lossy(), None, Merge::Newer);
            super::push_excludes(&mut args, &target.exclude);

            match self.run(&args, COMMAND_TIMEOUT).await {
                Ok(_) => outcomes.push(LocationOutcome::new(
                    target,
                    "pulled",
                    "已取回云端较新的文件",
                )),
                Err(error) => {
                    outcomes.push(LocationOutcome::new(target, "failed", error.to_string()))
                }
            }
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }
}

#[cfg(test)]
mod tests {
    use crate::sync::runner::testing::{CURRENT, FakeRclone, VERSIONS, target};

    #[tokio::test]
    async fn upload_sends_each_location_into_its_own_cloud_directory() {
        let fake = FakeRclone::new("upload");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(saves.join("nested")).unwrap();

        let mut target = target(&saves, "savedata", "rel-savedata");
        target.exclude = vec!["*.log".to_string(), "  ".to_string()];
        let outcome = fake
            .runner(false, 0)
            .upload("demo", "Demo", std::slice::from_ref(&target))
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "uploaded");

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let call = &calls[0];
        assert!(call.starts_with("copy "), "{call}");
        assert!(
            call.contains(&format!("copy {} {CURRENT}/rel-savedata", saves.display())),
            "{call}"
        );
        // Replaced files go aside so we keep history without a repo format.
        assert!(
            call.contains(&format!("--backup-dir {VERSIONS}/")),
            "{call}"
        );
        assert!(
            call.contains("/rel-savedata "),
            "per-location snapshot: {call}"
        );
        assert!(
            call.contains("--suffix  "),
            "empty suffix keeps names: {call}"
        );
        assert!(call.contains("--exclude *.log"), "{call}");
        assert!(
            !call.contains("--update"),
            "an upload must overwrite: {call}"
        );
        assert!(!call.contains("--delete"), "{call}");
        // Blank patterns are dropped rather than sent to rclone.
        assert!(!call.contains("--exclude   "), "{call}");
    }

    #[tokio::test]
    async fn a_location_that_does_not_exist_locally_is_reported_not_ignored() {
        let fake = FakeRclone::new("missing");
        let absent = fake.dir.join("never-created");
        let outcome = fake
            .runner(false, 0)
            .upload(
                "demo",
                "Demo",
                &[target(&absent, "savedata", "rel-savedata")],
            )
            .await;

        assert!(
            outcome.ok,
            "nothing to upload is not a failure: {outcome:?}"
        );
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(fake.calls().is_empty(), "no rclone call was needed");
    }

    #[tokio::test]
    async fn a_failing_location_is_named_and_does_not_hide_the_others() {
        let fake = FakeRclone::new("partial");
        let first = fake.dir.join("one");
        let second = fake.dir.join("two");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        fake.fail_on(&format!("copy {} ", first.display()));

        let outcome = fake
            .runner(false, 0)
            .upload(
                "demo",
                "Demo",
                &[
                    target(&first, "one", "rel-one"),
                    target(&second, "two", "rel-two"),
                ],
            )
            .await;

        assert!(!outcome.ok);
        assert_eq!(outcome.locations[0].action, "failed");
        assert_eq!(
            outcome.locations[1].action, "uploaded",
            "one bad location must not abort the rest"
        );
        assert!(outcome.error.unwrap().contains("one"));
    }

    #[tokio::test]
    async fn pulling_before_a_launch_can_never_overwrite_a_newer_local_save() {
        let fake = FakeRclone::new("pull");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "pulled");

        let call = &fake.calls()[1];
        assert!(call.contains(&format!("{CURRENT}/rel-savedata {}", saves.display())));
        // The whole point: an upload that failed earlier means the local copy is
        // newer, and it must win.
        assert!(call.contains("--update"), "{call}");
        assert!(
            !call.contains("--backup-dir"),
            "a pull must not create local versions: {call}"
        );
    }

    #[tokio::test]
    async fn a_game_the_cloud_has_never_seen_is_not_an_error() {
        let fake = FakeRclone::new("pull-empty");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(false, 0)
            .pull(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
            )
            .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "skipped");
        assert!(outcome.locations[0].detail.contains("云端还没有"));
        assert_eq!(fake.calls().len(), 1, "only the listing happened");
    }
}

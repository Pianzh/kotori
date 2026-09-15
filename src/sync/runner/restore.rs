//! 恢复与保留窗口：把云端某个时刻的存档铺回本机（`restore`），以及按滑动窗口
//! 删掉云上的旧快照（`prune`）。
//!
//! 与 `upload.rs` 分开：这里的每一步都可能覆盖本机数据，所以顺序很重要——
//! 先给自己留一份快照（`snapshot_now`），再覆盖，最后才谈清理；上传和拉取只做
//! 合并，不会删本机任何东西。

use super::{COMMAND_TIMEOUT, GameOutcome, LocationOutcome, Runner};
use crate::sync::{
    CURRENT_DIR, Merge, SaveTarget, SyncError, copy_args, game_remote, purge_args, restore_args,
    version_stamp, versions_remote,
};

impl Runner {
    /// Restore a game's saves.
    ///
    /// `version = None` restores the newest state. Naming a snapshot restores
    /// the state as it was *before* that upload: the snapshot holds the files
    /// that were replaced at the time, so it is overlaid on top of the current
    /// copy to rebuild that point in time.
    ///
    /// Before overwriting anything, the current local state is uploaded as a
    /// fresh snapshot (best effort). A restore is therefore itself undoable.
    pub async fn restore(
        &self,
        game_id: &str,
        name: &str,
        targets: &[SaveTarget],
        version: Option<&str>,
    ) -> GameOutcome {
        if let Err(error) = self.ready() {
            return GameOutcome::failed(game_id, name, error.to_string());
        }
        if let Some(version) = version
            && !super::is_snapshot(version)
        {
            return GameOutcome::failed(
                game_id,
                name,
                format!("不是合法的快照名: {version}（形如 20260911T101500Z）"),
            );
        }

        // Keep what we are about to replace.
        if let Err(error) = self.snapshot_now(game_id, targets).await {
            tracing::warn!("{}: 恢复前快照失败（继续恢复）: {error}", game_id);
        }

        let available = match self.current_keys(game_id).await {
            Ok(keys) => keys,
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

            let current = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
            let mut args = restore_args(&current, &target.local.to_string_lossy());
            super::push_excludes(&mut args, &target.exclude);

            if let Err(error) = self.run(&args, COMMAND_TIMEOUT).await {
                outcomes.push(LocationOutcome::new(target, "failed", error.to_string()));
                continue;
            }

            // Overlay the snapshot to get back to that point in time.
            if let Some(version) = version {
                let snapshot = format!(
                    "{}/{version}/{}",
                    versions_remote(&self.settings, game_id),
                    target.key
                );
                let mut args = restore_args(&snapshot, &target.local.to_string_lossy());
                super::push_excludes(&mut args, &target.exclude);
                if let Err(error) = self.run(&args, COMMAND_TIMEOUT).await {
                    outcomes.push(LocationOutcome::new(
                        target,
                        "failed",
                        format!("快照 {version} 叠加失败: {error}"),
                    ));
                    continue;
                }
                outcomes.push(LocationOutcome::new(
                    target,
                    "restored",
                    format!("已恢复到快照 {version}"),
                ));
            } else {
                outcomes.push(LocationOutcome::new(target, "restored", "已恢复到最新备份"));
            }
        }

        GameOutcome::from_locations(game_id, name, outcomes)
    }

    /// Upload the current state without touching the retention window.
    ///
    /// Used before a restore so the state being replaced is recoverable. It
    /// deliberately does not reuse [`Self::upload`]: that one prunes, and a
    /// restore must not be able to expire a snapshot as a side effect.
    async fn snapshot_now(&self, game_id: &str, targets: &[SaveTarget]) -> Result<(), SyncError> {
        let stamp = version_stamp(chrono::Utc::now());
        for target in targets {
            if !target.local.is_dir() {
                continue;
            }
            let destination = format!(
                "{}/{CURRENT_DIR}/{}",
                game_remote(&self.settings, game_id),
                target.key
            );
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
            self.run(&args, COMMAND_TIMEOUT).await?;
        }
        Ok(())
    }

    /// Delete the snapshots that fall outside the retention window.
    ///
    /// Never touches local files, and never touches a cloud directory that does
    /// not look like one of our own snapshots.
    pub async fn prune(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let stamps = self.versions(game_id).await?;
        let doomed = super::prune_plan(&stamps, self.settings.keep_versions);
        if doomed.is_empty() {
            return Ok(doomed);
        }

        let root = versions_remote(&self.settings, game_id);
        for stamp in &doomed {
            let args = purge_args(&format!("{root}/{stamp}"));
            self.run(&args, COMMAND_TIMEOUT).await?;
            tracing::info!("{game_id}: 已删除旧快照 {stamp}");
        }
        Ok(doomed)
    }
}

#[cfg(test)]
mod tests {
    use crate::sync::runner::testing::{CURRENT, FakeRclone, VERSIONS, target};

    #[tokio::test]
    async fn restoring_snapshots_what_it_is_about_to_replace() {
        let fake = FakeRclone::new("restore");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                None,
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.locations[0].action, "restored");

        let calls = fake.calls();
        // 1: the safety snapshot, 2: the listing, 3: the restore itself.
        assert!(
            calls[0].contains("--backup-dir"),
            "snapshot first: {calls:?}"
        );
        assert!(calls[1].starts_with("lsf"), "{calls:?}");
        assert!(
            calls[2].contains(&format!("{CURRENT}/rel-savedata {}", saves.display())),
            "{calls:?}"
        );
        assert!(
            !calls[2].contains("--update"),
            "an explicit restore is meant to win: {calls:?}"
        );
    }

    #[tokio::test]
    async fn restoring_a_snapshot_overlays_it_on_the_newest_state() {
        let fake = FakeRclone::new("restore-version");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        fake.set_listing(CURRENT, &["rel-savedata"]);

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("20260911T101500Z"),
            )
            .await;
        assert!(outcome.ok, "{outcome:?}");
        assert!(outcome.locations[0].detail.contains("20260911T101500Z"));

        let calls = fake.calls();
        // Current first, then the snapshot that holds the replaced files: a
        // snapshot on its own is only the diff of one upload.
        let current = calls
            .iter()
            .position(|c| c.contains(&format!("{CURRENT}/rel-savedata ")))
            .expect("current copy");
        let snapshot = calls
            .iter()
            .position(|c| c.contains(&format!("{VERSIONS}/20260911T101500Z/rel-savedata ")))
            .expect("snapshot overlay");
        assert!(current < snapshot, "{calls:?}");
    }

    #[tokio::test]
    async fn a_bogus_snapshot_name_is_refused_before_anything_is_touched() {
        let fake = FakeRclone::new("restore-bogus");
        let saves = fake.dir.join("saves");
        std::fs::create_dir_all(&saves).unwrap();

        let outcome = fake
            .runner(false, 0)
            .restore(
                "demo",
                "Demo",
                &[target(&saves, "savedata", "rel-savedata")],
                Some("../../../etc"),
            )
            .await;

        assert!(!outcome.ok);
        assert!(outcome.error.unwrap().contains("不是合法的快照名"));
        assert!(
            fake.calls().is_empty(),
            "nothing may run: {:?}",
            fake.calls()
        );
    }

    #[tokio::test]
    async fn retention_only_ever_purges_our_own_old_snapshots() {
        let fake = FakeRclone::new("prune");
        fake.set_listing(
            VERSIONS,
            &[
                "20260903T000000Z",
                "20260901T000000Z",
                "20260902T000000Z",
                "current",
                "not-ours",
            ],
        );

        let removed = fake.runner(false, 2).prune("demo").await.unwrap();
        assert_eq!(removed, vec!["20260901T000000Z".to_string()]);

        let purges = fake.calls_matching("purge");
        assert_eq!(purges.len(), 1, "{purges:?}");
        assert!(purges[0].contains(&format!("{VERSIONS}/20260901T000000Z")));
        assert!(
            !fake.env_log().is_empty(),
            "pruning still needs credentials in the environment"
        );
    }

    #[tokio::test]
    async fn retention_keeps_everything_unless_the_user_asked_otherwise() {
        let fake = FakeRclone::new("prune-off");
        fake.set_listing(VERSIONS, &["20260901T000000Z", "20260902T000000Z"]);

        assert!(
            fake.runner(false, 0)
                .prune("demo")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(fake.calls_matching("purge").is_empty());
        // Even with the window off, no listing is needed if keep is 0 — but if
        // it is, it must not delete anything it does not recognise.
        assert!(
            fake.runner(false, 5)
                .prune("demo")
                .await
                .unwrap()
                .is_empty()
        );
    }
}

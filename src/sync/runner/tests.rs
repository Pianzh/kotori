//! `Runner` 本身的单测：凭据怎么送出去、准备不足时什么时候停手。
//!
//! 四条路各自的测试在 `upload.rs` / `pull.rs` / `restore.rs` 里，这里只管
//! 传输底座与"云端有哪些包"。

use super::*;
use crate::sync::runner::testing::FakeRclone;

#[tokio::test]
async fn credentials_reach_rclone_through_the_environment_only() {
    let fake = FakeRclone::new("secrets");
    let outcome = fake.runner(0).check().await.unwrap();
    // 回话里带着"是哪个引擎、哪个二进制":两个引擎在桶里各写各的区域,
    // 设置页要能把这句话原样显示出来。
    assert!(
        outcome.ends_with(" · kotori:bkt/prefix"),
        "远端要报出来:{outcome}"
    );
    assert!(outcome.starts_with("rclone ("), "引擎要报出来:{outcome}");

    let calls = fake.calls();
    assert!(calls[0].starts_with("mkdir kotori:bkt/prefix"), "{calls:?}");
    // Nothing secret may ever appear on a command line: `ps` is world-read.
    for call in &calls {
        assert!(!call.contains("appkey456"), "{call}");
        assert!(!call.contains("keyid123"), "{call}");
    }

    let env = fake.env_log();
    assert!(
        env.contains("env:RCLONE_CONFIG_KOTORI_ACCOUNT=keyid123"),
        "{env}"
    );
    assert!(
        env.contains("env:RCLONE_CONFIG_KOTORI_KEY=appkey456"),
        "{env}"
    );
    assert!(env.contains("env:RCLONE_CONFIG=/dev/null"), "{env}");
    // Unencrypted setups carry no crypt remote at all.
    assert!(!env.contains("KOTORIENC"), "{env}");
}

#[tokio::test]
async fn missing_credentials_stop_the_run_before_rclone_is_started() {
    let fake = FakeRclone::new("no-secrets");
    let runner = Runner::with_binary(&fake.bin, fake.settings(0), Keyring::memory());
    let outcome = runner.upload("demo", "Demo", &[]).await;

    assert!(!outcome.ok);
    assert!(outcome.error.unwrap().contains("B2 凭据"));
    assert!(fake.calls().is_empty());
}

#[test]
fn a_missing_endpoint_means_rclone_picks_one() {
    // The native B2 backend is happy with no endpoint, and that is the
    // normal case; the S3 endpoint the B2 console shows is a different API
    // and is rejected before a run ever starts (see `sync::validate`).
    let fake = FakeRclone::new("no-endpoint");
    let settings = fake.settings(0);
    assert!(settings.endpoint.is_empty());
    assert!(crate::sync::validate(&settings).is_ok());
}

#[tokio::test]
async fn a_disabled_sync_config_is_refused() {
    let fake = FakeRclone::new("disabled");
    let mut settings = fake.settings(0);
    settings.enabled = false;
    let runner = Runner::with_binary(&fake.bin, settings, fake.keyring());

    assert!(matches!(runner.ready(), Err(SyncError::NotEnabled)));
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn the_newest_package_is_the_largest_name() {
    let fake = FakeRclone::new("packages");
    for stamp in [
        "20260901T000000Z-aaaa1111",
        "20260903T000000Z-bbbb2222",
        "20260902T000000Z-cccc3333",
    ] {
        fake.put(&format!("kotori:bkt/prefix/games/demo/{stamp}.zip"), "old");
    }
    // 别的对象（不是我们的包）不算版本：保留窗口只认自己认得的名字。
    fake.put("kotori:bkt/prefix/games/demo/notes.txt", "not ours");

    let runner = fake.runner(0);
    let packages = runner.packages("demo").await.unwrap();
    assert_eq!(packages.len(), 3, "{packages:?}");
    assert_eq!(
        runner.latest_package("demo").await.unwrap(),
        Some("20260903T000000Z-bbbb2222".to_string())
    );

    // 云端一个包都没有：不是错误，是"还没上传过"。
    assert!(runner.packages("never-uploaded").await.unwrap().is_empty());
    assert!(
        runner
            .latest_package("never-uploaded")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_scratch_directory_is_cleaned_up_even_after_a_failure() {
    let fake = FakeRclone::new("scratch");
    let saves = fake.dir.join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    std::fs::write(saves.join("save.sav"), "one").unwrap();
    let target = crate::sync::runner::testing::target(&saves, "savedata", "rel-savedata");

    let runner = fake.runner(0);
    runner
        .upload("demo", "Demo", std::slice::from_ref(&target))
        .await;
    fake.fail_on("copyto ");
    runner.upload("demo", "Demo", &[target]).await;

    let leftover: Vec<String> = std::fs::read_dir(fake.dir.join("work"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftover.is_empty(),
        "临时目录必须自己收拾干净，失败也不例外: {leftover:?}"
    );
}

//! 凭据那一半的 `sync.*` 单测:四级存储的写入/主密码/删除,以及"显式给进来的
//! 存储不许被悄悄换掉"。从 `tests.rs` 搬来(纯移动)—— 那边曾越过 500 行硬线。

use super::super::*;
use super::SettingsPatch;
use super::tests::{call, daemon, daemon_config};
use crate::secrets::testing::FakeTool;
use crate::secrets::{Keyring, SecretKey};

#[cfg(unix)]
#[tokio::test]
async fn credentials_go_into_the_keyring_and_can_be_cleared() {
    let fake = FakeTool::new("credentials");
    let keyring = fake.keyring();
    let daemon = daemon(keyring.clone());
    let credentials = |body: &'static str| {
        let daemon = &daemon;
        async move { call(daemon, "sync.set_credentials", body).await }
    };

    // Half a credential is refused: it can never work.
    let value = credentials(r#"{"key_id":"id","app_key":"  "}"#).await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("一起填"),
        "{value}"
    );

    let value = credentials(r#"{"key_id":"keyid","app_key":"appkey"}"#).await;
    assert_eq!(value["result"]["stored"], true);
    assert_eq!(
        keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("keyid")
    );
    // Values are trimmed: copied credentials often carry whitespace.
    let value = credentials(r#"{"key_id":" keyid2 ","app_key":" appkey2 "}"#).await;
    assert_eq!(value["result"]["stored"], true);
    assert_eq!(
        keyring.get(SecretKey::B2AppKey).unwrap().as_deref(),
        Some("appkey2")
    );

    let value = credentials(r#"{"key_id":"","app_key":""}"#).await;
    assert_eq!(value["result"]["cleared"], true);
    assert!(keyring.present().is_empty());
}

#[tokio::test]
async fn a_master_password_seals_the_credentials_and_survives_a_restart() {
    // The whole point of this backend: on a machine with no OS keyring the
    // B2 keys must not have to be retyped after every reboot, or unattended
    // sync is impossible there.
    let dir = std::env::temp_dir().join(format!(
        "kotori-master-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("secrets.json");

    // --- a machine with nothing but a session store ---------------------
    let daemon = Daemon::with_keyring_at(daemon_config(), Keyring::memory(), path.clone());
    call(
        &daemon,
        "sync.set_credentials",
        r#"{"key_id":"005keyid","app_key":"K005appkey"}"#,
    )
    .await;

    let value = call(
        &daemon,
        "sync.set_master_password",
        r#"{"password":"correct horse battery","force":true}"#,
    )
    .await;
    assert_eq!(value["result"]["stored"], true, "{value}");
    assert_eq!(value["result"]["count"], 2);
    assert!(path.is_file(), "凭据文件应当被创建");

    // The plaintext must not be anywhere in the file.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("K005appkey"), "{raw}");

    // And the daemon is now using that file, unlocked.
    let status = call(&daemon, "sync.status", "").await;
    assert_eq!(
        status["result"]["keyring"]["store"]["kind"],
        "encrypted-file"
    );
    assert_eq!(status["result"]["keyring"]["store"]["locked"], false);
    // ⚠ 不要断言 `ready`:它还要求 PATH 上有 rclone(`ready = enabled &&
    // problem.is_none() && rclone.is_some()`),而这条测试讲的是凭据本身 ——
    // 在没装 rclone 的机器上(CI 就是)它会红得毫无道理。凭据在不在,
    // 看 `secrets` 与 `ephemeral` 就够了。
    assert_eq!(status["result"]["secrets"].as_array().unwrap().len(), 2);
    assert_eq!(status["result"]["keyring"]["ephemeral"], false, "{status}");

    // --- the same machine after a restart ------------------------------
    let restarted = Daemon::with_master_file(daemon_config(), path.clone());
    let status = call(&restarted, "sync.status", "").await;
    assert_eq!(
        status["result"]["keyring"]["store"]["kind"],
        "encrypted-file"
    );
    assert_eq!(
        status["result"]["keyring"]["store"]["locked"], true,
        "重启后应当是锁定的"
    );
    assert!(
        status["result"]["problem"]
            .as_str()
            .unwrap()
            .contains("已锁定"),
        "锁定时要说「已锁定」，不能说「还没有凭据」: {status}"
    );
    // The credentials are still there — just not readable yet.
    assert!(status["result"]["secrets"].as_array().unwrap().is_empty());

    // A wrong password changes nothing.
    let value = call(&restarted, "sync.unlock", r#"{"password":"nope"}"#).await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("主密码"),
        "{value}"
    );
    assert_eq!(
        call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
        true
    );

    // The right one opens it, and the credentials come back.
    let value = call(
        &restarted,
        "sync.unlock",
        r#"{"password":"correct horse battery"}"#,
    )
    .await;
    assert_eq!(value["result"]["unlocked"], true, "{value}");
    let status = call(&restarted, "sync.status", "").await;
    assert_eq!(status["result"]["keyring"]["store"]["locked"], false);
    assert_eq!(status["result"]["secrets"].as_array().unwrap().len(), 2);
    // 同上:这里证明"解锁之后凭据回来了",不是"这台机器能同步"。
    assert_eq!(status["result"]["keyring"]["ephemeral"], false, "{status}");

    // Locking again hides them without destroying anything.
    let value = call(&restarted, "sync.lock", "").await;
    assert_eq!(value["result"]["locked"], true, "{value}");
    assert_eq!(
        call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
        true
    );
    // ...and unlocking still works, so nothing was thrown away.
    call(
        &restarted,
        "sync.unlock",
        r#"{"password":"correct horse battery"}"#,
    )
    .await;
    assert_eq!(
        call(&restarted, "sync.status", "").await["result"]["secrets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 忘了主密码时的出路:删凭据文件**不需要先解锁**(GUI 的「删除凭据文件」就走这条,
/// 所以它必须一直可用 —— 否则锁着的文件就成了删不掉的垃圾)。
#[tokio::test]
async fn the_master_file_can_be_deleted_even_while_it_is_locked() {
    let dir = std::env::temp_dir().join(format!(
        "kotori-master-clear-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("secrets.json");

    let daemon = Daemon::with_keyring_at(daemon_config(), Keyring::memory(), path.clone());
    call(
        &daemon,
        "sync.set_credentials",
        r#"{"key_id":"005keyid","app_key":"K005appkey"}"#,
    )
    .await;
    call(
        &daemon,
        "sync.set_master_password",
        r#"{"password":"correct horse battery","force":true}"#,
    )
    .await;
    assert!(path.is_file(), "凭据文件应当被创建");

    // 重启后文件是锁着的 —— 正是"忘了主密码"的那种状态。
    let restarted = Daemon::with_master_file(daemon_config(), path.clone());
    assert_eq!(
        call(&restarted, "sync.status", "").await["result"]["keyring"]["store"]["locked"],
        true
    );

    let value = call(&restarted, "sync.clear_master_password", "").await;
    assert_eq!(value["result"]["removed"], true, "{value}");
    assert!(!path.is_file(), "文件应当被删掉");

    // 没有文件后端了,而且如实说现在用哪一级 —— 但不断言是哪一级:
    // 这台机器上有没有真密钥环不是这个测试能决定的。
    let status = call(&restarted, "sync.status", "").await;
    assert_ne!(
        status["result"]["keyring"]["store"]["kind"], "encrypted-file",
        "文件都没了,不该还说自己在用文件后端: {status}"
    );

    // 再删一次要明说"没有文件",而不是假装成功。
    let value = call(&restarted, "sync.clear_master_password", "").await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("没有主密码凭据文件"),
        "{value}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_short_master_password_is_refused_with_a_reason() {
    let dir = std::env::temp_dir().join(format!(
        "kotori-master-short-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let daemon =
        Daemon::with_keyring_at(daemon_config(), Keyring::memory(), dir.join("secrets.json"));
    call(
        &daemon,
        "sync.set_credentials",
        r#"{"key_id":"k","app_key":"s"}"#,
    )
    .await;

    let value = call(
        &daemon,
        "sync.set_master_password",
        r#"{"password":"short","force":true}"#,
    )
    .await;
    assert!(
        value["error"]["message"].as_str().unwrap().contains("至少"),
        "{value}"
    );
    assert!(!dir.join("secrets.json").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn kopia_password_can_be_stored_and_cleared() {
    let keyring = Keyring::memory();
    let daemon = daemon(keyring.clone());

    let stored = call(
        &daemon,
        "sync.set_kopia_password",
        r#"{"password":"custom-repository-password"}"#,
    )
    .await;
    assert_eq!(stored["result"]["stored"], true, "{stored}");
    assert_eq!(stored["result"]["using_default"], false, "{stored}");
    assert_eq!(
        keyring.get(SecretKey::KopiaPassword).unwrap().as_deref(),
        Some("custom-repository-password")
    );

    let cleared = call(&daemon, "sync.set_kopia_password", r#"{"password":""}"#).await;
    assert_eq!(cleared["result"]["cleared"], true, "{cleared}");
    assert_eq!(cleared["result"]["using_default"], true, "{cleared}");
    assert_eq!(keyring.get(SecretKey::KopiaPassword).unwrap(), None);
}

#[test]
fn an_explicitly_given_store_is_never_replaced_behind_the_caller() {
    // Only the *fallback* is retried. A store handed in deliberately (tests,
    // and any future backend) must stay in use, otherwise every test that
    // seeds a fake keyring would silently start reading the real one.
    let keyring = Keyring::memory();
    keyring.set(SecretKey::B2KeyId, "seeded").unwrap();
    let state = SyncState::with_keyring(keyring);

    for _ in 0..3 {
        let current = state.keyring();
        assert_eq!(
            current.get(SecretKey::B2KeyId).unwrap().as_deref(),
            Some("seeded")
        );
        assert!(current.is_ephemeral());
    }
}

#[test]
fn the_store_never_changes_under_a_running_daemon() {
    // 这里从前断言的是"内存后端能把凭据交给后来起来的密钥环" —— 那条路已删
    // (明文文件永远是兜底 ⇒ 生产路径上没有 session-only 这一级)。现在要钉住的是
    // 剩下那件事实:`keyring()` 每次给的都是**同一个**存储,不会背着调用方换。
    let state = SyncState::with_keyring(Keyring::memory());
    state.keyring().set(SecretKey::B2KeyId, "id").unwrap();
    assert_eq!(
        state.keyring().get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("id")
    );
    // 换成别的存储是显式动作(`adopt`),不是"下次问就自己变了"。
    state.adopt(Keyring::plain_file(std::path::Path::new(
        "/nonexistent/creds.json",
    )));
    assert!(state.keyring().get(SecretKey::B2KeyId).unwrap().is_none());
}

#[tokio::test]
async fn a_launch_with_sync_off_does_not_touch_the_network() {
    let fake = FakeTool::new("launch-off");
    let mut config = Config::default();
    config.sync.enabled = false;
    let daemon = Daemon::with_keyring(config, fake.keyring());
    assert!(
        daemon.sync_pull_before_launch("demo").await.is_none(),
        "nothing to do, and nothing to report"
    );
}

#[tokio::test]
async fn a_launch_never_fails_because_sync_is_broken() {
    let fake = FakeTool::new("launch-broken");
    let daemon = daemon(fake.keyring());

    // Sync is on but nothing is set up and there is no rclone: the launch
    // must still get an answer it can act on.
    let report = daemon
        .sync_pull_before_launch("demo")
        .await
        .expect("a report");
    assert_eq!(report["ok"], false);
    assert!(report["error"].is_string(), "{report}");
}

/// `[sync]` 里**每一个**键，`SettingsPatch` 都必须收得下；它不认识的键必须当场报错。
///
/// 这条盯着"界面加了新设置、daemon 忘了跟上"。少了 `deny_unknown_fields`，serde 对不
/// 认识的键**默认不吭声** —— 调用方发了等于没发，还拿到 `success: true`（写
/// `kopia_binary` 时真踩过一次）。加上之后"缺字段"就不再是静默的，这条测试于是能把
/// 它照出来：喂进去的键要么被认下，要么明确报 unknown field。
///
/// ⚠ 它管不了"字段收了、却忘了写应用代码"那种：那种 bug 得看值有没有真的变，
/// 见 `tests/ipc_e2e/sync.rs::every_sync_setting_can_actually_be_changed`。
#[test]
fn every_sync_config_key_is_accepted_by_the_settings_patch() {
    let config = serde_json::to_value(crate::config::SyncConfig::default()).unwrap();
    let fields = config.as_object().expect("[sync] 该是个表");
    assert!(!fields.is_empty(), "SyncConfig 不该是空的");

    // 值的类型也一起过：键收了、类型收不下，一样是"改不了"。
    for (key, sample) in fields {
        let mut body = serde_json::Map::new();
        body.insert(key.clone(), sample.clone());
        serde_json::from_value::<SettingsPatch>(Value::Object(body))
            .unwrap_or_else(|e| panic!("daemon 收不下 [sync] 的 `{key}`：{e}"));
    }

    // 反过来那一半：多一个键（界面上拼错一个字母）必须报错，而不是被当成没问题。
    let err = serde_json::from_value::<SettingsPatch>(serde_json::json!({ "kopia_binry": "/x" }))
        .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

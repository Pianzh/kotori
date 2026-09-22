//! `sync.*` RPC 的单测：设置怎么校验、状态里能出现什么、以及凭据那几级存储
//! 各自的行为。

use super::super::*;
// `SettingsPatch` 住在 `sync_rpc` 自己那一层（`pub(super)`），不随 `daemon::*` 过来。
use crate::config::{Config, GameConfig, SavePath, ScaleProfile};
use crate::secrets::Keyring;
#[cfg(unix)]
use crate::secrets::SecretKey;
use crate::secrets::testing::FakeTool;
use std::path::PathBuf;

/// Send a raw JSON-RPC request through the real dispatcher.
pub(super) async fn call(daemon: &Daemon, method: &str, params: &str) -> Value {
    let request = if params.is_empty() {
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#)
    } else {
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#)
    };
    let reply = daemon.handle_request(&request).await;
    serde_json::from_str(&reply.body).unwrap_or_else(|e| panic!("bad reply {}: {e}", reply.body))
}

/// The sync settings every test here starts from.
pub(super) fn daemon_config() -> Config {
    let mut config = Config::default();
    config.sync.enabled = true;
    config.sync.bucket = "bkt".to_string();
    config
}

pub(super) fn daemon(keyring: Keyring) -> Daemon {
    daemon_at(keyring).0
}

/// 同上，但把配置文件的路径也交出来 —— 单测要断言"写下去的东西真的落盘了"。
pub(super) fn daemon_at(keyring: Keyring) -> (Daemon, PathBuf) {
    let mut config = Config::default();
    config.sync.enabled = true;
    config.sync.endpoint = String::new();
    config.sync.bucket = "bkt".to_string();
    config.games.insert(
        "demo".into(),
        GameConfig {
            cloud_id: None,
            exe_fingerprint: None,
            cloud_dir: None,
            cloud_rejected: Vec::new(),
            name: "demo".into(),
            game_dir: PathBuf::from("/games/demo"),
            exe_path: PathBuf::from("/games/demo/game.exe"),
            launch_args: Vec::new(),
            save_paths: vec![SavePath::inferred("savedata")],
            wine_prefix: None,
            auto_watch: false,
            direct_launch: false,
            process_name: None,
            scale_profile: ScaleProfile::default_for(),
            created_at: chrono::Utc::now(),
        },
    );
    // Tests own a throw-away config file: the daemon persists every settings
    // change, and the machine-wide config is not theirs to touch.
    let dir = std::env::temp_dir().join(format!(
        "kotori-sync-rpc-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("config.toml");
    (
        Daemon::with_keyring(config, keyring).with_config_path(path.clone()),
        path,
    )
}

// 假 secret-tool(FakeTool)是 shell 脚本,Unix 限定——Windows 的密钥环后端
// 还没实现,这两条在 Windows 上 spawn 不出来(os error 193)。
#[cfg(unix)]
#[tokio::test]
async fn status_never_returns_a_secret_value() {
    let fake = FakeTool::new("status-secrets");
    let keyring = fake.keyring();
    keyring.set(SecretKey::B2KeyId, "keyid123").unwrap();
    keyring.set(SecretKey::B2AppKey, "appkey456").unwrap();

    let daemon = daemon(keyring);
    let value = call(&daemon, "sync.status", "").await;
    let body = value.to_string();
    let result = &value["result"];
    assert_eq!(result["enabled"], true);
    assert_eq!(result["settings"]["bucket"], "bkt");
    assert_eq!(result["remote"], "kotori:bkt/kotori");
    // Names only: the UI has no business holding the key.
    assert_eq!(result["secrets"][0], "b2-key-id");
    assert!(!body.contains("keyid123"), "{body}");
    assert!(!body.contains("appkey456"), "{body}");
    // 同步密码随 crypt 层一起没了：status 里也不该再有它的取回提示。
    assert!(result.get("password_hint").is_none(), "{result}");
    assert_eq!(result["games"][0]["locations"], 1);
}

#[tokio::test]
async fn status_explains_what_is_missing_instead_of_failing() {
    let fake = FakeTool::new("status-missing");
    let value = call(&daemon(fake.keyring()), "sync.status", "").await;
    assert_eq!(value["result"]["ready"], false);
    assert!(
        value["result"]["problem"]
            .as_str()
            .unwrap()
            .contains("B2 凭据"),
        "{value}"
    );
}

#[tokio::test]
async fn settings_are_validated_before_they_are_stored() {
    let fake = FakeTool::new("settings");
    let daemon = daemon(fake.keyring());

    // An endpoint the backend cannot use, and paths that could escape the
    // prefix or the bucket.
    for (body, needle) in [
        // The B2 console shows the S3 endpoint first, and it is the wrong
        // one for this backend; say so instead of failing later with a 404.
        (
            r#"{"endpoint":"s3.us-west-004.backblazeb2.com"}"#,
            "S3 兼容接口",
        ),
        // rclone does not add a scheme, so a bare host cannot work.
        (r#"{"endpoint":"api001.backblazeb2.com"}"#, "https://"),
        (r#"{"bucket":"my/bucket"}"#, "bucket"),
        (r#"{"prefix":"../other"}"#, ".."),
        (r#"{"prefix":"  "}"#, "prefix"),
        (r#"{"keep_versions":100000}"#, "最多"),
        // Enabling with nowhere to sync to is refused.
        (r#"{"enabled":true,"bucket":""}"#, "bucket"),
    ] {
        let value = call(&daemon, "sync.set_settings", body).await;
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(needle)),
            "expected {needle:?} in {value}"
        );
    }

    // Turning sync off and clearing the fields together is fine: an
    // unfinished setup is only a problem while sync is on.
    let value = call(
        &daemon,
        "sync.set_settings",
        r#"{"enabled":false,"endpoint":""}"#,
    )
    .await;
    assert_eq!(value["result"]["settings"]["enabled"], false);
    assert_eq!(value["result"]["settings"]["endpoint"], "");
    let value = call(&daemon, "sync.set_settings", r#"{"enabled":true}"#).await;
    assert_eq!(value["result"]["settings"]["enabled"], true);

    // A good patch is stored, and persisted.
    let value = call(
        &daemon,
        "sync.set_settings",
        r#"{"bucket":"new-bucket","prefix":"/saves/kotori/"}"#,
    )
    .await;
    assert_eq!(value["result"]["settings"]["bucket"], "new-bucket");
    assert_eq!(
        value["result"]["settings"]["prefix"], "saves/kotori",
        "the prefix is normalised, not rejected"
    );
}

/// 本机的机器身份：一台机器只有一个，而且**落盘**（重开进程读到的必须是同一个）。
#[tokio::test]
async fn the_machine_identity_is_created_once_and_persisted() {
    let fake = FakeTool::new("machine-id");
    let (daemon, config_path) = daemon_at(fake.keyring());
    assert_eq!(daemon.config.read().await.daemon.machine_id, None);

    let first = daemon.machine_id().await.unwrap();
    assert_eq!(first.len(), 36, "机器身份是个 uuid: {first}");
    assert_eq!(daemon.machine_id().await.unwrap(), first, "一台机器一个");

    let persisted = crate::config::load_at(&config_path).unwrap();
    assert_eq!(
        persisted.daemon.machine_id.as_deref(),
        Some(first.as_str()),
        "身份卡上要拿它对账，所以必须落盘"
    );
}

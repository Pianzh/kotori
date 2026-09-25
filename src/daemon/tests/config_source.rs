//! 「切到便携 / 切到默认」这一族:配置搬过去,凭据跟着走(BUG-27)。
//!
//! 从 `daemon/tests.rs` 拆出来 —— 那边加上"改配置前重读磁盘"那条回归之后正好顶到
//! 500 行软线(用户 2026-09-25 定的规矩:500 是软线、600 才是硬线,软线不该随意越)。

use super::*;

#[tokio::test]
async fn switching_the_config_source_moves_it_and_makes_the_portable_one_step_aside() {
    let dir = crate::config::test_scratch("daemon-switch");
    let portable = dir.join("exe_dir").join("config.toml");
    let default = dir.join("config_dir").join("config.toml");
    std::fs::create_dir_all(portable.parent().unwrap()).unwrap();

    // daemon 站在"默认目录"这一份上,内存里有一条游戏(它就是被搬过去的内容)。
    let config: Config = toml::from_str(
        r#"
[games.probe]
name = "探针"
exe_path = "/games/probe/game.exe"
"#,
    )
    .unwrap();
    crate::config::save_to(&default, &config).unwrap();
    // 凭据也钉在临时目录里:切换来源会连着凭据一起搬(BUG-27),不能让它去碰这台
    // 机器上真实的那一份(`Daemon::new` 会走到真密钥环)。
    let daemon = Daemon::with_keyring_at(
        config,
        crate::secrets::Keyring::plain_file(dir.join("config_dir").join("credentials.json")),
        dir.join("config_dir").join("secrets.json"),
    )
    .with_config_path(default.clone())
    .with_config_sources(Some(portable.clone()), default.clone());

    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set_source","params":{"portable":true}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["changed"], true, "{value}");
    assert!(portable.is_file(), "便携那份应当被写出来:{value}");
    assert!(default.is_file(), "切到便携时默认那份不许动");
    let moved = crate::config::load_from(&portable).unwrap();
    assert!(moved.games.contains_key("probe"), "搬过去的是内存里那一份");

    // 再切回默认:便携那份必须让路(改名),否则下次启动还是它赢。
    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":2,"method":"config.set_source","params":{"portable":false}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["changed"], true, "{value}");
    assert!(!portable.is_file(), "便携那份应当让路:{value}");
    assert!(portable.with_extension("toml.portable-bak").is_file());
    assert_eq!(
        value["result"]["config_path"],
        default.display().to_string(),
        "{value}"
    );

    // daemon 记住的就是新路径:下一次写配置落在默认那份上(唯一写者换了地方)。
    daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":3,"method":"game.update","params":{"id":"probe","name":"改过名"}}"#,
        )
        .await;
    assert!(crate::config::load_from(&default).unwrap().games["probe"].name == "改过名");
    assert!(!portable.is_file(), "写回去也不许悄悄重建便携那份");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 两个地点一样(或已经在那儿)时不写文件、不报错 —— 按钮那时本来就是灰的,而
/// RPC 也得自己站得住。
#[tokio::test]
async fn switching_to_the_source_already_in_use_changes_nothing() {
    let dir = crate::config::test_scratch("daemon-switch-same");
    let default = dir.join("config.toml");
    crate::config::save_to(&default, &Config::default()).unwrap();
    let daemon = Daemon::with_keyring_at(
        Config::default(),
        crate::secrets::Keyring::plain_file(dir.join("credentials.json")),
        dir.join("secrets.json"),
    )
    .with_config_path(default.clone())
    .with_config_sources(Some(default.clone()), default.clone());

    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set_source","params":{"portable":true}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["changed"], false, "{value}");
    assert!(default.is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 切配置来源时凭据**跟着走**(BUG-27):配置搬到便携目录,凭据也搬过去 ——
/// 便携的全部意义就是"整个目录拷走就能用",而系统密钥环里的东西带不走。
#[tokio::test]
async fn switching_the_config_source_takes_the_credentials_along() {
    let dir = crate::config::test_scratch("daemon-credentials-move");
    let config_dir = dir.join("config_dir");
    let portable_dir = dir.join("exe_dir");
    let default = config_dir.join("config.toml");
    let portable = portable_dir.join("config.toml");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&portable_dir).unwrap();
    crate::config::save_to(&default, &Config::default()).unwrap();

    // 默认地点那一份:明文文件,里面存着一条 B2 key。
    let store = crate::secrets::Keyring::plain_file(config_dir.join("credentials.json"));
    store
        .set(crate::secrets::SecretKey::B2KeyId, "key-id")
        .unwrap();
    let daemon = Daemon::with_keyring_at(Config::default(), store, config_dir.join("secrets.json"))
        .with_config_path(default.clone())
        .with_config_sources(Some(portable.clone()), default.clone());

    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set_source","params":{"portable":true}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert!(value.get("error").is_none(), "{value}");

    // 凭据落在便携目录旁边,内容原样。
    let moved = portable_dir.join("credentials.json");
    assert!(moved.is_file(), "凭据没跟着配置走:{value}");
    assert_eq!(
        crate::secrets::Keyring::plain_file(moved)
            .get(crate::secrets::SecretKey::B2KeyId)
            .unwrap(),
        Some("key-id".to_string())
    );
    // 状态页报的也得是新路径 —— 从前它永远报 daemon 启动时算的那个(BUG-27)。
    assert!(
        daemon.sync.secrets_path().starts_with(&portable_dir),
        "状态页还指着老地方:{:?}",
        daemon.sync.secrets_path()
    );
    // 切回默认:凭据跟着回来。
    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":2,"method":"config.set_source","params":{"portable":false}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(value["result"]["changed"], true, "{value}");
    assert!(
        daemon.sync.secrets_path().starts_with(&config_dir),
        "切回来之后凭据该回到默认目录:{:?}",
        daemon.sync.secrets_path()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failed_credential_move_leaves_the_config_source_unchanged() {
    let dir = crate::config::test_scratch("daemon-switch-failure");
    let default_dir = dir.join("default");
    let default = default_dir.join("config.toml");
    std::fs::create_dir_all(&default_dir).unwrap();
    crate::config::save_to(&default, &Config::default()).unwrap();

    // 便携目录的父亲故意是一个普通文件，搬凭据时会在创建目录阶段失败。
    let blocked_parent = dir.join("blocked");
    std::fs::write(&blocked_parent, b"not a directory").unwrap();
    let portable = blocked_parent.join("config.toml");

    let store = crate::secrets::Keyring::plain_file(default_dir.join("credentials.json"));
    store
        .set(crate::secrets::SecretKey::B2KeyId, "key-id")
        .unwrap();
    let daemon =
        Daemon::with_keyring_at(Config::default(), store, default_dir.join("secrets.json"))
            .with_config_path(default.clone())
            .with_config_sources(Some(portable.clone()), default.clone());
    let before = std::fs::read_to_string(&default).unwrap();

    let reply = daemon
        .handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"config.set_source","params":{"portable":true}}"#,
        )
        .await;
    let value: Value = serde_json::from_str(&reply.body).unwrap();
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("切换配置来源失败"),
        "{value}"
    );
    assert_eq!(*daemon.config_path.read().await, default);
    assert_eq!(std::fs::read_to_string(&default).unwrap(), before);
    assert!(!portable.exists());
    assert!(daemon.sync.secrets_path().starts_with(&default_dir));

    let _ = std::fs::remove_dir_all(&dir);
}

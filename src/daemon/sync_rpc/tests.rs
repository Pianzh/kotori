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
            game_dir_mount: None,
            exe_mount: None,
            exe_fingerprint: None,
            cloud_dir: None,
            cloud_rejected: Vec::new(),
            sync_enabled: true,
            cloud_conclusion: None,
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
    // 先把这份配置**写进那个文件**:daemon 改配置时会先重读磁盘(见
    // `Daemon::mutate_config`),而"内存里有、磁盘上没有"的 daemon 在生产里不
    // 存在 —— 启动时它就是从这个文件读出来的。
    crate::config::save_to(&path, &config).unwrap();
    (
        Daemon::with_keyring(config, keyring).with_config_path(path.clone()),
        path,
    )
}

/// `sync.status` 不许把**配置读锁**跨着 `await` 持有 —— 那会和写请求形成**自死锁**。
///
/// 用户 2026-09-26 报的"点『给这一款新建一条』之后卡在启动中"就是它（整台 daemon 一起
/// 僵住）。交错是这样的：`sync.status` 先拿到配置读锁，然后带着它去读云端索引
/// （`cloud_index_view`），而那一步自己**还要再取一次**读锁；此时写请求（`sync.resolve`）
/// 在写锁上排队，tokio 读写锁的公平性让那次**新的读**也排在写者后面 —— 读者等的是自己
/// 手里的锁，两边都回不来，之后所有读配置的请求一起排队。
///
/// 这条测试把那个窗口**撑成确定的**：读者的路上有一把同步锁（`records`），测试先把它攥住，
/// 读者就停在"已经持有读锁"的状态上；这时放写请求进来（它会排在写锁上），最后松开那把
/// 同步锁。修复前：读者继续往前走、去取第二次读锁、排在写者后面 —— 死锁成立。
/// `timeout` 收口，所以它只会失败，不会把 CI 挂住。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_status_does_not_hold_the_config_lock_across_its_await() {
    let fake = FakeTool::new("sync-status-lock");
    let (daemon, _path) = daemon_at(fake.keyring());
    let daemon = std::sync::Arc::new(daemon);

    // ① 攥住 `records`：`sync.status` 会停在它上面 —— 那一刻它已经拿着配置读锁。
    let held = daemon.sync.records.lock().unwrap();

    // ② 让读者跑起来，并给它足够时间停在那把锁上。
    let reader = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.rpc_sync_status().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // ③ 放一个写请求进来：它会在配置写锁上排队。
    let writer = {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            daemon
                .mutate_config(|config| {
                    config.sync.enabled = !config.sync.enabled;
                    Ok(Value::Null)
                })
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // ④ 松开：读者继续走，去取它那第二次读锁 —— 修复前这里就再也回不来了。
    drop(held);

    let finished = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let _ = reader.await;
        let _ = writer.await;
    })
    .await;

    assert!(
        finished.is_ok(),
        "sync.status 与写请求撞在一起时死锁了：配置读锁被跨着 await 持有"
    );
}

/// 没有存档位置就开不了云同步 —— 用户 2026-09-26 定的规则：**身份是身份，路径是路径**。
///
/// 界面把那颗开关置灰（`ui::render::detail`），但规则得在 daemon 上成立 —— 界面之外还有
/// CLI 和别的客户端。允许"只填身份、不填路径"，代价就是开不了同步；而**身份没填照样能开**，
/// 那时启动会弹那一问，所以"弹窗"这条路上必然已经有路径了。
#[tokio::test]
async fn cloud_sync_cannot_be_switched_on_without_a_save_location() {
    let fake = FakeTool::new("sync-needs-a-path");
    let (daemon, path) = daemon_at(fake.keyring());

    // 造出"没有存档位置、同步也关着"的那一款。改的是**磁盘上**那份：daemon 改配置时
    // 先重读磁盘（见 `Daemon::mutate_config`），内存里那份不算数。
    let rewrite = |save_paths: Vec<SavePath>, sync_enabled: bool| {
        let mut on_disk = crate::config::load_at(&path).unwrap();
        let game = on_disk.games.get_mut("demo").unwrap();
        game.save_paths = save_paths;
        game.sync_enabled = sync_enabled;
        crate::config::save_to(&path, &on_disk).unwrap();
    };
    rewrite(Vec::new(), false);

    // 想开：拒绝，并说清原因。
    let refused = call(&daemon, "game.update", r#"{"id":"demo","sync_enabled":true}"#).await;
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("存档位置"), "{refused}");
    assert!(
        !daemon.config.read().await.games["demo"].sync_enabled,
        "被拒之后开关不该留下"
    );

    // 填上路径之后就能开 —— 校验看的是"这一刻"的档案，不认死哪一次请求。
    rewrite(vec![SavePath::inferred("savedata")], false);
    let allowed = call(&daemon, "game.update", r#"{"id":"demo","sync_enabled":true}"#).await;
    assert_eq!(allowed["result"]["success"], true, "{allowed}");
    assert!(daemon.config.read().await.games["demo"].sync_enabled);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
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

/// 每款一个的云同步开关：关掉这一款 ⇒ **不自动**取回 / 上传；手动那条路不受它限制。
#[tokio::test]
async fn switching_one_game_off_stops_its_automatic_sync_only() {
    let fake = FakeTool::new("per-game-switch");
    let (daemon, _) = daemon_at(fake.keyring());

    // 开着（默认）：启动前那条路会去干活（这里凭据是齐的，所以给得出回话）。
    assert!(
        daemon.sync_pull_before_launch("demo").await.is_some(),
        "开关开着时，启动前该走同步那条路"
    );

    // 用户把这一款关掉。
    let value = call(
        &daemon,
        "game.update",
        r#"{"id":"demo","sync_enabled":false}"#,
    )
    .await;
    assert_eq!(value["result"]["success"], true, "{value}");
    assert!(!daemon.config.read().await.games["demo"].sync_enabled);

    // 自动取回：一个字都不做（回话是 `None` = 没什么可报的）。
    assert!(
        daemon.sync_pull_before_launch("demo").await.is_none(),
        "关掉之后不许自动取回"
    );

    // 手动「立即同步」是用户自己按的：不受这个开关限制。
    let value = call(&daemon, "sync.now", r#"{"id":"demo"}"#).await;
    assert!(
        value["result"].is_object() || value["error"].is_object(),
        "手动同步这一条路不该被开关拦掉: {value}"
    );

    // 再打开：开关就是个开关，不是一次性的。
    let value = call(
        &daemon,
        "game.update",
        r#"{"id":"demo","sync_enabled":true}"#,
    )
    .await;
    assert_eq!(value["result"]["success"], true, "{value}");
    assert!(daemon.config.read().await.games["demo"].sync_enabled);
}

/// 启动前的自检与"用户答了什么"：三种回答各自的效果，以及**答过就不许再问**。
#[tokio::test]
async fn the_pre_launch_self_check_asks_once_and_remembers_the_answer() {
    use crate::sync::selfcheck::Decision;

    let fake = FakeTool::new("selfcheck");
    let (daemon, _) = daemon_at(fake.keyring());
    let signature = crate::sync::signature::of(&daemon.config.read().await.sync).unwrap();

    // 新档案、没有指纹：认不出云端那一条 ⇒ **问一次**（不带"疑似找到的那一条"）。
    assert_eq!(
        daemon.sync_selfcheck("demo").await,
        Decision::Ask { found: None }
    );

    // "自己挑一条绑上"：绑上云端那一条 ⇒ 结论是"已确认"，而且**真的绑着**。
    let value = call(
        &daemon,
        "sync.resolve",
        r#"{"id":"demo","choice":"pair","cloud_id":"cloud-1","cloud_key":"demo"}"#,
    )
    .await;
    assert_eq!(value["result"]["ok"], true, "{value}");
    {
        let config = daemon.config.read().await;
        assert_eq!(config.games["demo"].cloud_id.as_deref(), Some("cloud-1"));
        assert_eq!(
            config.games["demo"].cloud_conclusion.as_deref(),
            Some(format!("ok:{signature}").as_str())
        );
    }
    // 真的绑上了 ⇒ 下次不问、也不重扫（`Pull` 那条路一个字节都不读云端）。
    assert_eq!(daemon.sync_selfcheck("demo").await, Decision::Pull);

    // 换了目标（桶）：结论作废，回到"未定"。没有指纹时照样是"问一次"。
    call(
        &daemon,
        "sync.set_settings",
        r#"{"bucket":"another-bucket"}"#,
    )
    .await;
    assert_eq!(
        daemon.sync_selfcheck("demo").await,
        Decision::Ask { found: None }
    );

    // "关掉这一款的同步"：只关这一款，而且记住"问过了"。
    let value = call(&daemon, "sync.resolve", r#"{"id":"demo","choice":"off"}"#).await;
    assert_eq!(value["result"]["ok"], true, "{value}");
    {
        let config = daemon.config.read().await;
        assert!(!config.games["demo"].sync_enabled);
        let conclusion = config.games["demo"].cloud_conclusion.clone().unwrap();
        assert!(conclusion.starts_with("off:"), "{conclusion}");
    }
    // 关着的时候打开游戏：一个字都不做（不再问第二次）。
    assert_eq!(daemon.sync_selfcheck("demo").await, Decision::Skip);

    // 用户自己把这一款重新打开：**上次那份结论被清掉**（用户 2026-09-24："之后我不论开关
    // 云同步都不会再次弹窗，这也是问题"），于是下一次启动重新自检 —— 指纹认不出就是
    // "再问一次"，不再是"直接新建、再也不问"。
    call(
        &daemon,
        "game.update",
        r#"{"id":"demo","sync_enabled":true}"#,
    )
    .await;
    {
        let config = daemon.config.read().await;
        assert!(
            config.games["demo"].cloud_conclusion.is_none(),
            "重新打开要把上次那份结论清掉"
        );
    }
    assert_eq!(
        daemon.sync_selfcheck("demo").await,
        Decision::Ask { found: None }
    );
}

#[tokio::test]
async fn resolve_can_bind_an_existing_cloud_identity() {
    let (daemon, path) = daemon_at(Keyring::memory());
    let value = call(
        &daemon,
        "sync.resolve",
        r#"{"id":"demo","choice":"pair","cloud_id":"cloud-1","cloud_key":"remote/demo"}"#,
    )
    .await;
    assert_eq!(value["result"]["ok"], true, "{value}");

    let config = crate::config::load_from(&path).unwrap();
    let game = &config.games["demo"];
    assert_eq!(game.cloud_id.as_deref(), Some("cloud-1"));
    assert_eq!(game.cloud_dir.as_deref(), Some("remote/demo"));
    assert!(
        game.cloud_conclusion
            .as_deref()
            .is_some_and(|value| value.starts_with("ok:"))
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[tokio::test]
async fn resolve_pair_without_an_identity_creates_a_new_binding_decision() {
    let (daemon, path) = daemon_at(Keyring::memory());
    let value = call(&daemon, "sync.resolve", r#"{"id":"demo","choice":"pair"}"#).await;
    assert_eq!(value["result"]["ok"], true, "{value}");

    let config = crate::config::load_from(&path).unwrap();
    let game = &config.games["demo"];
    assert!(game.cloud_id.is_none());
    assert!(game.cloud_dir.is_none());
    assert!(
        game.cloud_conclusion
            .as_deref()
            .is_some_and(|value| value.starts_with("new:"))
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[tokio::test]
async fn resolve_rejects_an_unknown_choice_without_mutating_the_binding() {
    let (daemon, path) = daemon_at(Keyring::memory());
    let before = crate::config::load_from(&path).unwrap();

    let value = call(
        &daemon,
        "sync.resolve",
        r#"{"id":"demo","choice":"unknown"}"#,
    )
    .await;
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("不认识的回答"),
        "{value}"
    );

    let after = crate::config::load_from(&path).unwrap();
    assert_eq!(after.games["demo"].cloud_id, before.games["demo"].cloud_id);
    assert_eq!(
        after.games["demo"].cloud_dir,
        before.games["demo"].cloud_dir
    );
    assert_eq!(
        after.games["demo"].cloud_conclusion,
        before.games["demo"].cloud_conclusion
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[tokio::test]
async fn repeating_the_same_pair_is_idempotent() {
    let (daemon, path) = daemon_at(Keyring::memory());
    let body = r#"{"id":"demo","choice":"pair","cloud_id":"cloud-1","cloud_key":"remote/demo"}"#;

    let first = call(&daemon, "sync.resolve", body).await;
    let second = call(&daemon, "sync.resolve", body).await;
    assert_eq!(first["result"]["ok"], true, "{first}");
    assert_eq!(second["result"]["ok"], true, "{second}");

    let config = crate::config::load_from(&path).unwrap();
    let game = &config.games["demo"];
    assert_eq!(game.cloud_id.as_deref(), Some("cloud-1"));
    assert_eq!(game.cloud_dir.as_deref(), Some("remote/demo"));
    assert!(
        game.cloud_conclusion
            .as_deref()
            .is_some_and(|value| value.starts_with("ok:"))
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

//! `secrets` 的单测：四级存储各自的脾气，以及"现在到底存在哪一级"的说法。

// 这两个只在 unix 测试里用(windows 的密钥环后端还没实现,那几条测试整条
// `#[cfg(unix)]`,见下面的 `FakeTool`)。
#[cfg(unix)]
use super::keyring::adopt_plain_entries;
#[cfg(unix)]
use super::plain::PlainFile;
// 假 secret-tool 是 shell 脚本,只在 Unix 上能跑 —— 用到它的测试全部
// #[cfg(unix)],Windows 的密钥环后端(凭据管理器)实现后再补那边的夹具。
#[cfg(unix)]
use super::testing::FakeTool;
use super::*;

#[cfg(unix)]
#[test]
fn round_trips_a_secret_through_the_keyring() {
    let fake = FakeTool::new("roundtrip");
    let keyring = fake.keyring();

    assert_eq!(keyring.get(SecretKey::B2AppKey).unwrap(), None);

    keyring
        .set(SecretKey::B2AppKey, "hunter2 with spaces")
        .unwrap();
    assert_eq!(
        keyring.get(SecretKey::B2AppKey).unwrap(),
        Some("hunter2 with spaces".to_string()),
        "values are passed verbatim, whitespace included"
    );

    // The stored value is what the user would see with plain secret-tool.
    assert_eq!(
        std::fs::read_to_string(fake.stored(SecretKey::B2AppKey)).unwrap(),
        "hunter2 with spaces"
    );

    keyring.clear(SecretKey::B2AppKey).unwrap();
    assert_eq!(keyring.get(SecretKey::B2AppKey).unwrap(), None);
}

#[cfg(unix)]
#[test]
fn secrets_are_stored_under_distinct_accounts() {
    let fake = FakeTool::new("distinct");
    let keyring = fake.keyring();
    keyring.set(SecretKey::B2KeyId, "keyid").unwrap();
    keyring.set(SecretKey::B2AppKey, "appkey").unwrap();

    assert_eq!(
        keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("keyid")
    );
    assert_eq!(
        keyring.get(SecretKey::B2AppKey).unwrap().as_deref(),
        Some("appkey")
    );

    // 三条各占一个 account：存一条绝不能踩掉另一条（kopia 密码与 B2 的 key 是
    // 互相独立的两件事，前者没有就用默认值，后者必须有）。
    assert_ne!(SecretKey::B2KeyId.account(), SecretKey::B2AppKey.account());
    assert_ne!(
        SecretKey::B2KeyId.account(),
        SecretKey::KopiaPassword.account()
    );
    assert_ne!(
        SecretKey::B2AppKey.account(),
        SecretKey::KopiaPassword.account()
    );
    assert_eq!(SecretKey::ALL.len(), 3, "B2 两条加 kopia 密码");
}

#[cfg(unix)]
#[test]
fn an_installed_but_dead_backend_is_not_mistaken_for_a_working_one() {
    // The bug this covers: `secret-tool` was present, so the settings page
    // said the keyring was fine, while every store silently failed and
    // every read looked like "nothing saved yet".
    let fake = FakeTool::broken("dead");
    let keyring = fake.keyring();

    let error = keyring.probe().unwrap_err();
    assert!(
        matches!(error, SecretError::BackendNotRunning { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("没有在运行"), "{message}");
    // It has to say what to do about it, not just that it is broken...
    assert!(message.contains("密钥环"), "{message}");
    // ...and say it in the terms of *this* system. Advice for another
    // platform is worse than useless: it sends the user chasing a service
    // that does not exist there.
    #[cfg(target_os = "linux")]
    assert!(
        !message.contains("凭据管理器"),
        "Windows 的建议不该出现在 Linux 上: {message}"
    );
    #[cfg(windows)]
    {
        assert!(!message.contains("secret-tool"), "{message}");
        assert!(!message.contains("kwalletd6"), "{message}");
    }

    // A read must not masquerade as "nothing stored".
    let error = keyring.get(SecretKey::B2KeyId).unwrap_err();
    assert!(
        matches!(error, SecretError::BackendNotRunning { .. }),
        "{error:?}"
    );

    // And a write reports the real reason too.
    let error = keyring.set(SecretKey::B2AppKey, "x").unwrap_err();
    assert!(
        matches!(error, SecretError::BackendNotRunning { .. }),
        "{error:?}"
    );

    // `system_or_memory` is what the daemon uses, so it must degrade to a
    // session store rather than refusing to run at all.
    let memory = Keyring::memory();
    assert!(memory.is_ephemeral() && memory.probe().is_ok());
}

#[cfg(unix)]
#[test]
fn a_healthy_backend_answers_the_probe_without_finding_anything() {
    let fake = FakeTool::new("probe-ok");
    let keyring = fake.keyring();
    assert!(keyring.probe().is_ok(), "the entry simply does not exist");
    assert!(
        keyring.get(SecretKey::B2AppKey).unwrap().is_none(),
        "a healthy store still reports a missing entry as None"
    );
}

#[test]
fn missing_backend_is_reported_instead_of_falling_back_to_plaintext() {
    let keyring = Keyring::with_tool("/nonexistent/secret-tool");
    let error = keyring.get(SecretKey::B2AppKey).unwrap_err();
    assert!(matches!(error, SecretError::Command(_)), "{error:?}");
}

#[test]
fn system_detection_reports_what_is_on_this_machine() {
    // The real answer depends on the machine; the contract is that it either
    // yields a usable handle or a clear "no keyring here" error.
    match Keyring::system() {
        Ok(keyring) => assert!(!keyring.is_ephemeral()),
        Err(error) => {
            // Any of the three "not usable here" states is a valid answer
            // depending on the machine.
            assert!(
                matches!(
                    error,
                    SecretError::BackendMissing
                        | SecretError::BackendUnsupported(_)
                        | SecretError::BackendNotRunning { .. }
                ),
                "{error:?}"
            );
        }
    }
}

/// **策略(2026-09-13)**:没有密钥环时落点是**明文 0600 文件**,不是内存。
///
/// 这条测试以前断言的是反过来的东西("绝不退化成明文")—— 用户明确改了这个决定:
/// 日常工具(opencode / gh)都是明文 0600,不该为了一个 B2 key 逼用户输主密码。
/// 现在硬性的部分变成:**必须能持久化,而且必须是 0600**。
///
/// "没有密钥环"是**注入**的,不是指望这台机器恰好没有:Plasma 会话里
/// `ksecretd` 是被桌面拉起来的,KDE 上跑就会走到密钥环那一支(那是对的行为,
/// 不是这条测试要断言的东西)。见 [`Keyring::open_default_with`]。
#[test]
fn a_machine_without_a_keyring_falls_back_to_a_private_plain_file() {
    let dir = std::env::temp_dir().join(format!(
        "kotori-plain-default-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let encrypted = dir.join("secrets.json");
    let plain = dir.join("credentials.json");

    let store = Keyring::open_default_with(Err(SecretError::BackendMissing), &encrypted, &plain);
    assert!(!store.is_ephemeral(), "明文文件是能持久化的,不该报成临时的");
    assert_eq!(
        store.kind(),
        StoreKind::PlainFile {
            path: plain.display().to_string()
        }
    );

    store.set(SecretKey::B2KeyId, "005keyid").unwrap();
    assert!(plain.is_file(), "写下去就该有文件");
    assert_eq!(
        store.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("005keyid")
    );
    // 换一个句柄重开(等价于守护进程重启),凭据要还在。
    let reopened = Keyring::open_default_with(Err(SecretError::BackendMissing), &encrypted, &plain);
    assert_eq!(
        reopened.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("005keyid")
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&plain).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "明文文件必须 0600，实际 {mode:o}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// 有密钥环时:明文里已有的凭据要**搬进去**,搬全了就把明文删掉 —— 能不留明文就不留。
/// (用户 2026-09-13:"密钥环作为可选使用"。)
#[cfg(unix)]
#[test]
fn a_keyring_that_shows_up_takes_over_the_plaintext_file() {
    let fake = FakeTool::new("takeover");
    let keyring = fake.keyring();
    let dir = std::env::temp_dir().join(format!(
        "kotori-takeover-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let plain = dir.join("credentials.json");
    let file = PlainFile::new(&plain);
    file.store(&[(SecretKey::B2KeyId, "005keyid".to_string())])
        .unwrap();

    adopt_plain_entries(&keyring, &plain);

    assert_eq!(
        keyring.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("005keyid"),
        "明文里的凭据必须搬进密钥环,不能像凭空消失了一样"
    );
    assert!(!plain.exists(), "搬全了就该删掉明文");
    std::fs::remove_dir_all(&dir).ok();
}

/// 挑选顺序本身:密钥环在跑时它赢,明文里的东西被搬走后不再留明文。
/// (顺序的另外两档——加密文件优先、都没有则明文——由上面两条测试覆盖。)
#[cfg(unix)]
#[test]
fn a_running_keyring_wins_over_the_plaintext_file() {
    let fake = FakeTool::new("wins");
    let dir = std::env::temp_dir().join(format!(
        "kotori-wins-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let encrypted = dir.join("secrets.json");
    let plain = dir.join("credentials.json");
    PlainFile::new(&plain)
        .store(&[(SecretKey::B2KeyId, "005keyid".to_string())])
        .unwrap();

    let store = Keyring::open_default_with(Ok(fake.keyring()), &encrypted, &plain);

    assert!(
        matches!(store.kind(), StoreKind::System { .. }),
        "{:?}",
        store.kind()
    );
    assert_eq!(
        store.get(SecretKey::B2KeyId).unwrap().as_deref(),
        Some("005keyid"),
        "切到密钥环不能把已有凭据弄丢"
    );
    assert!(!plain.exists(), "搬全了就该删掉明文");
    std::fs::remove_dir_all(&dir).ok();
}

/// 加密文件是用户显式选过的更严那一级,它比密钥环更优先。
/// (Unix 限定:它借假密钥环当"在跑的那一级"用;Windows 没有密钥环后端,
/// 这个优先级问题在那边不成立。)
#[cfg(unix)]
#[test]
fn an_encrypted_file_wins_over_a_running_keyring() {
    let fake = FakeTool::new("encrypted-first");
    let dir = std::env::temp_dir().join(format!(
        "kotori-encfirst-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let encrypted = dir.join("secrets.json");
    let plain = dir.join("credentials.json");
    // 一个存在(锁着)的加密文件就够:选它不需要密码,解锁是后面的事。
    EncryptedFile::new(&encrypted)
        .create(
            "a-password-long-enough",
            &[(SecretKey::B2KeyId, "005keyid".to_string())],
        )
        .unwrap();

    let store = Keyring::open_default_with(Ok(fake.keyring()), &encrypted, &plain);

    assert!(
        matches!(store.kind(), StoreKind::EncryptedFile { .. }),
        "{:?}",
        store.kind()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_machine_without_a_keyring_keeps_secrets_in_memory_only() {
    let keyring = Keyring::memory();
    assert!(keyring.is_ephemeral());
    assert!(keyring.get(SecretKey::B2AppKey).unwrap().is_none());

    keyring.set(SecretKey::B2AppKey, "hunter2").unwrap();
    assert_eq!(
        keyring.get(SecretKey::B2AppKey).unwrap().as_deref(),
        Some("hunter2")
    );
    assert_eq!(keyring.present(), vec![SecretKey::B2AppKey]);
    // The memory store never shells out, so it cannot leak through a file.
    assert!(keyring.run(&["lookup"], None).is_err());

    keyring.clear(SecretKey::B2AppKey).unwrap();
    assert!(keyring.present().is_empty());
    // Clearing twice is not an error, exactly like the real keyring.
    keyring.clear(SecretKey::B2AppKey).unwrap();
}

#[test]
fn the_store_describes_itself_honestly() {
    assert!(Keyring::memory().describe().contains("内存"));
    let keyring = Keyring::with_tool("/usr/bin/secret-tool");
    assert!(keyring.describe().contains("/usr/bin/secret-tool"));
    assert!(!keyring.describe().contains("内存"));
}

/// 提示语在任何平台上都不能是空的 —— 那是用户唯一能看到的解释。
#[test]
fn every_hint_has_something_to_say() {
    assert!(!backend_name().is_empty());
    assert!(!keyring_hint().is_empty());
}

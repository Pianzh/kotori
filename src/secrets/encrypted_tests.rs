//! `encrypted` 的单元测试：主密码文件、密封与解封、坏文件的处置。
//!
//! 从 `encrypted.rs` 拆出来 —— 那边连着测试一起数越过了 500 行的硬线（AGENTS.md）。

use super::*;

struct TempFile(PathBuf);

impl TempFile {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kotori-cred-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Self(dir.join("secrets.json"))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            std::fs::remove_dir_all(parent).ok();
        }
    }
}

fn entries() -> Vec<(SecretKey, String)> {
    vec![
        (SecretKey::B2KeyId, "0046b52121c35c50000000001".to_string()),
        (SecretKey::B2AppKey, "K004secret".to_string()),
    ]
}

#[test]
fn a_round_trip_keeps_every_value_exactly() {
    let temp = TempFile::new("roundtrip");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();

    let mut loaded = store.load().unwrap();
    loaded.sort_by_key(|(key, _)| key.account());
    let mut expected = entries();
    expected.sort_by_key(|(key, _)| key.account());
    assert_eq!(loaded, expected);
}

#[test]
fn the_file_contains_no_sign_of_the_secrets() {
    let temp = TempFile::new("opaque");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();

    let raw = std::fs::read_to_string(&temp.0).unwrap();
    for (_, value) in entries() {
        assert!(!raw.contains(&value), "明文泄漏到文件里: {raw}");
    }
    // Nor the account names, which are inside the sealed payload too.
    assert!(!raw.contains("b2-app-key"), "{raw}");
    assert!(raw.contains("argon2id"), "KDF 参数应当可读: {raw}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&temp.0).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "凭据文件必须是 0600");
    }
}

#[test]
fn a_wrong_password_is_refused_and_leaves_the_store_locked() {
    let temp = TempFile::new("wrongpass");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();
    store.lock();

    let error = store.unlock("Correct horse battery").unwrap_err();
    assert!(matches!(error, SecretError::WrongMasterPassword), "{error}");
    assert!(!store.is_unlocked());
    // Reading without unlocking is a distinct, actionable state.
    assert!(matches!(store.load(), Err(SecretError::Locked)));
}

#[test]
fn locking_forgets_the_key_but_not_the_data() {
    let temp = TempFile::new("lock");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();

    // A fresh handle stands in for the next process start.
    let reopened = EncryptedFile::new(&temp.0);
    assert!(reopened.exists());
    assert!(!reopened.is_unlocked());
    assert!(matches!(reopened.load(), Err(SecretError::Locked)));

    reopened.unlock("correct horse battery").unwrap();
    assert_eq!(reopened.load().unwrap().len(), 2);

    reopened.lock();
    assert!(!reopened.is_unlocked());
    reopened.unlock("correct horse battery").unwrap();
    assert_eq!(reopened.load().unwrap().len(), 2);
}

#[test]
fn writing_replaces_the_contents_without_losing_the_password() {
    let temp = TempFile::new("update");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();

    store
        .store(&[(SecretKey::B2KeyId, "rotated".to_string())])
        .unwrap();

    let reopened = EncryptedFile::new(&temp.0);
    reopened.unlock("correct horse battery").unwrap();
    let loaded = reopened.load().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, SecretKey::B2KeyId);
    assert_eq!(loaded[0].1, "rotated");
}

#[test]
fn every_write_uses_a_fresh_nonce() {
    // Reusing a nonce under the same key breaks the AEAD, so this is not a
    // detail we can leave to chance.
    let temp = TempFile::new("nonce");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();
    let first: Envelope = serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();

    store.store(&entries()).unwrap();
    let second: Envelope = serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();

    assert_ne!(first.nonce, second.nonce);
    assert_ne!(first.ciphertext, second.ciphertext);
}

#[test]
fn tampering_with_the_parameters_or_the_payload_is_detected() {
    let temp = TempFile::new("tamper");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();

    // Downgrade the work factor: the header is authenticated, so this must
    // not silently unlock.
    let mut envelope: Envelope = serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();
    envelope.m_cost = 8;
    std::fs::write(&temp.0, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let reopened = EncryptedFile::new(&temp.0);
    let error = reopened.unlock("correct horse battery").unwrap_err();
    assert!(matches!(error, SecretError::WrongMasterPassword), "{error}");

    // Flip a byte of the ciphertext.
    let mut envelope: Envelope = serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();
    envelope.m_cost = M_COST;
    let mut bytes = from_hex(&envelope.ciphertext).unwrap();
    bytes[0] ^= 0x01;
    envelope.ciphertext = to_hex(&bytes);
    std::fs::write(&temp.0, serde_json::to_vec(&envelope).unwrap()).unwrap();

    assert!(matches!(
        EncryptedFile::new(&temp.0).unlock("correct horse battery"),
        Err(SecretError::WrongMasterPassword)
    ));
}

#[test]
fn a_short_password_is_refused_before_anything_is_written() {
    let temp = TempFile::new("short");
    let store = EncryptedFile::new(&temp.0);
    let error = store.create("short", &entries()).unwrap_err();
    assert!(
        matches!(error, SecretError::MasterPasswordTooShort { .. }),
        "{error}"
    );
    assert!(!store.exists(), "拒绝之后不该留下半个文件");
}

#[test]
fn a_missing_file_reads_as_locked_rather_than_corrupt() {
    let temp = TempFile::new("absent");
    let store = EncryptedFile::new(&temp.0);
    assert!(!store.exists());
    assert!(matches!(store.load(), Err(SecretError::Locked)));
    // Removing something that is not there is not an error.
    assert!(store.remove().is_ok());
}

#[test]
fn rekeying_replaces_the_password() {
    let temp = TempFile::new("rekey");
    let store = EncryptedFile::new(&temp.0);
    store.create("correct horse battery", &entries()).unwrap();
    store.create("a different passphrase", &entries()).unwrap();

    assert!(matches!(
        EncryptedFile::new(&temp.0).unlock("correct horse battery"),
        Err(SecretError::WrongMasterPassword)
    ));
    let reopened = EncryptedFile::new(&temp.0);
    reopened.unlock("a different passphrase").unwrap();
    assert_eq!(reopened.load().unwrap().len(), 2);
}

#[test]
fn hex_round_trips_and_rejects_nonsense() {
    assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
    assert_eq!(from_hex("000fff"), Some(vec![0x00, 0x0f, 0xff]));
    assert_eq!(from_hex(""), Some(Vec::new()));
    assert_eq!(from_hex("abc"), None, "odd length");
    assert_eq!(from_hex("zz"), None, "not hex");
}

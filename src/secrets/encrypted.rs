//! Credentials in a file, sealed with a password the user chooses.
//!
//! This is the fallback for machines that have no OS keyring: a session that
//! never started one (niri, sway, a container), and — once the Windows port
//! lands — any system where the platform store is unavailable. On Windows the
//! Credential Manager is always there, so this file is for everywhere else.
//!
//! Rules it follows, and why:
//!   * **The password is the user's.** We never generate one: a password the
//!     user has never seen turns their backup into something only this program
//!     can open (ADR-010). It is never written anywhere — only a key derived
//!     from it is, and only in memory.
//!   * **No cryptography of our own.** Key derivation is Argon2id and sealing
//!     is ChaCha20-Poly1305, both from RustCrypto. Hand-rolled crypto is how
//!     the rclone `obscure` mistake happened (ADR-010).
//!   * **The header is authenticated.** Version and KDF parameters are fed in
//!     as associated data, so an attacker cannot quietly downgrade the work
//!     factor to make guessing cheaper.
//!   * **Losing the password loses the file.** That is the honest consequence
//!     of encrypting at rest, and the reason this is a *fallback*: wherever a
//!     real keyring exists, we use it instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, ParamsBuilder, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};

use super::{SecretError, SecretKey};

/// Bumped whenever the on-disk shape changes; old files are read by version.
const FORMAT_VERSION: u32 = 1;
const KDF_NAME: &str = "argon2id";
/// Argon2id cost: 19 MiB, two passes, one lane — the OWASP minimum, which is
/// about 50 ms here. Enough to make guessing expensive, small enough that
/// unlocking at startup is not noticeable.
const M_COST: u32 = 19 * 1024;
const T_COST: u32 = 2;
const P_COST: u32 = 1;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// Shortest master password we accept. Not a security guarantee, just a floor
/// against a typo becoming the lock on the user's own data.
pub const MIN_MASTER_PASSWORD: usize = 8;

/// The on-disk envelope. Only `ciphertext` is secret; everything else is bound
/// to it as associated data.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    version: u32,
    kdf: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    /// Hex; not secret, just unique per file.
    salt: String,
    /// Hex; one per write, never reused.
    nonce: String,
    /// Hex of the sealed `{"<account>": "<value>"}` map.
    ciphertext: String,
}

impl Envelope {
    /// Everything that must not be tampered with, in a fixed order.
    fn associated_data(&self) -> Vec<u8> {
        format!(
            "kotori-secrets|v{}|{}|m{}|t{}|p{}",
            self.version, self.kdf, self.m_cost, self.t_cost, self.p_cost
        )
        .into_bytes()
    }
}

/// A master-password-sealed credential file.
///
/// Cheap to clone (the path and the unlocked key are shared), which is what
/// lets the daemon hand it to whichever task needs it.
#[derive(Debug, Clone)]
pub struct EncryptedFile {
    path: PathBuf,
    /// The derived key, present only while unlocked. Never persisted.
    key: std::sync::Arc<Mutex<Option<[u8; KEY_LEN]>>>,
}

impl EncryptedFile {
    /// Point at a file without touching it.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            key: std::sync::Arc::new(Mutex::new(None)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    pub fn is_unlocked(&self) -> bool {
        self.key.lock().map(|key| key.is_some()).unwrap_or(false)
    }

    /// Forget the key. The file is untouched; a locked store simply cannot be
    /// read until the password is entered again.
    pub fn lock(&self) {
        if let Ok(mut key) = self.key.lock() {
            *key = None;
        }
    }

    /// Create (or re-key) the file with this password.
    ///
    /// Used both for the first setup and for changing the password: the latter
    /// is the same operation, since the whole file is re-sealed.
    pub fn create(
        &self,
        password: &str,
        entries: &[(SecretKey, String)],
    ) -> Result<(), SecretError> {
        let mut password = password.to_string();
        if password.len() < MIN_MASTER_PASSWORD {
            return Err(SecretError::MasterPasswordTooShort {
                minimum: MIN_MASTER_PASSWORD,
            });
        }

        let salt = random_bytes::<SALT_LEN>()?;
        let key = derive_key(&password, &salt)?;
        // Do not keep the plaintext password around longer than the derivation.
        password.clear();

        let mut envelope = Envelope {
            version: FORMAT_VERSION,
            kdf: KDF_NAME.to_string(),
            m_cost: M_COST,
            t_cost: T_COST,
            p_cost: P_COST,
            salt: to_hex(&salt),
            nonce: to_hex(&random_bytes::<NONCE_LEN>()?),
            ciphertext: String::new(),
        };
        envelope.ciphertext = to_hex(&seal(&key, &envelope, &encode_entries(entries))?);

        write_private(&self.path, &encode_envelope(&envelope)?)?;
        if let Ok(mut stored) = self.key.lock() {
            *stored = Some(key);
        }
        Ok(())
    }

    /// Read the file with this password and keep the key for later reads.
    pub fn unlock(&self, password: &str) -> Result<(), SecretError> {
        let envelope = self.read_envelope()?;
        let salt = from_hex(&envelope.salt).ok_or_else(|| {
            SecretError::Crypto("凭据文件里的 salt 不是合法的十六进制".to_string())
        })?;
        let key = derive_key(password, &salt)?;

        // Decrypting *is* the password check: the AEAD tag only verifies under
        // the right key.
        self.open(&envelope, &key)?;
        if let Ok(mut stored) = self.key.lock() {
            *stored = Some(key);
        }
        Ok(())
    }

    /// Everything in the file. Requires an unlocked store.
    pub fn load(&self) -> Result<Vec<(SecretKey, String)>, SecretError> {
        let envelope = self.read_envelope()?;
        let key = self.unlocked_key()?;
        let plain = self.open(&envelope, &key)?;
        decode_entries(&plain)
    }

    /// Replace the contents. Requires an unlocked store.
    pub fn store(&self, entries: &[(SecretKey, String)]) -> Result<(), SecretError> {
        let mut envelope = self.read_envelope()?;
        let key = self.unlocked_key()?;
        // A fresh nonce on every write: reusing one under the same key would
        // break the AEAD.
        envelope.nonce = to_hex(&random_bytes::<NONCE_LEN>()?);
        envelope.ciphertext = to_hex(&seal(&key, &envelope, &encode_entries(entries))?);
        write_private(&self.path, &encode_envelope(&envelope)?)
    }

    /// Delete the file and forget the key.
    pub fn remove(&self) -> Result<(), SecretError> {
        self.lock();
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SecretError::Io(error.to_string())),
        }
    }

    fn read_envelope(&self) -> Result<Envelope, SecretError> {
        let raw = std::fs::read(&self.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SecretError::Locked
            } else {
                SecretError::Io(format!("{}: {error}", self.path.display()))
            }
        })?;
        let envelope: Envelope = serde_json::from_slice(&raw)
            .map_err(|e| SecretError::Crypto(format!("凭据文件解析失败: {e}")))?;
        if envelope.version != FORMAT_VERSION {
            return Err(SecretError::Crypto(format!(
                "凭据文件版本是 {}，这个版本的程序只认识 {FORMAT_VERSION}",
                envelope.version
            )));
        }
        Ok(envelope)
    }

    fn unlocked_key(&self) -> Result<[u8; KEY_LEN], SecretError> {
        self.key
            .lock()
            .ok()
            .and_then(|key| *key)
            .ok_or(SecretError::Locked)
    }

    fn open(&self, envelope: &Envelope, key: &[u8; KEY_LEN]) -> Result<Vec<u8>, SecretError> {
        let ciphertext = from_hex(&envelope.ciphertext)
            .ok_or_else(|| SecretError::Crypto("密文不是合法的十六进制".to_string()))?;

        let cipher = ChaCha20Poly1305::new_from_slice(key)
            .map_err(|e| SecretError::Crypto(format!("初始化加密器失败: {e}")))?;
        let nonce = nonce_of(envelope)?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: &envelope.associated_data(),
                },
            )
            // A tag failure is overwhelmingly "wrong password", so that is what
            // we report; a corrupt file looks the same from here.
            .map_err(|_| SecretError::WrongMasterPassword)
    }
}

/// The nonce, as the cipher wants it.
fn nonce_of(envelope: &Envelope) -> Result<Nonce, SecretError> {
    let bytes = from_hex(&envelope.nonce)
        .ok_or_else(|| SecretError::Crypto("nonce 不是合法的十六进制".to_string()))?;
    Nonce::try_from(bytes.as_slice()).map_err(|_| SecretError::Crypto("nonce 长度不对".to_string()))
}

fn encode_envelope(envelope: &Envelope) -> Result<Vec<u8>, SecretError> {
    serde_json::to_vec(envelope).map_err(|e| SecretError::Crypto(format!("序列化失败: {e}")))
}

fn seal(
    key: &[u8; KEY_LEN],
    envelope: &Envelope,
    plaintext: &[u8],
) -> Result<Vec<u8>, SecretError> {
    let nonce = nonce_of(envelope)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| SecretError::Crypto(format!("初始化加密器失败: {e}")))?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &envelope.associated_data(),
            },
        )
        .map_err(|e| SecretError::Crypto(format!("加密失败: {e}")))
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], SecretError> {
    let params = ParamsBuilder::new()
        .m_cost(M_COST)
        .t_cost(T_COST)
        .p_cost(P_COST)
        .output_len(KEY_LEN)
        .build()
        .map_err(|e| SecretError::Crypto(format!("Argon2 参数无效: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; KEY_LEN];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| SecretError::Crypto(format!("派生密钥失败: {e}")))?;
    Ok(key)
}

/// The plaintext is a JSON object keyed by the keyring account name, so the
/// file is self-describing to anyone who later holds the password.
fn encode_entries(entries: &[(SecretKey, String)]) -> Vec<u8> {
    let map: std::collections::BTreeMap<&str, &str> = entries
        .iter()
        .map(|(key, value)| (key.account(), value.as_str()))
        .collect();
    serde_json::to_vec(&map).unwrap_or_else(|_| b"{}".to_vec())
}

fn decode_entries(plain: &[u8]) -> Result<Vec<(SecretKey, String)>, SecretError> {
    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(plain)
        .map_err(|e| SecretError::Crypto(format!("凭据内容解析失败: {e}")))?;
    Ok(map
        .into_iter()
        .filter_map(|(account, value)| {
            SecretKey::ALL
                .into_iter()
                .find(|key| key.account() == account)
                .map(|key| (key, value))
        })
        .collect())
}

/// Write with owner-only permissions, atomically.
///
/// The temporary file matters: a half-written credential file would lock the
/// user out of their own secrets.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), SecretError> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SecretError::Io(e.to_string()))?;
    }
    let temp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temp).map_err(|e| SecretError::Io(e.to_string()))?;
        // Set the mode before anything is written, so the plaintext-shaped
        // bytes are never briefly world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| SecretError::Io(e.to_string()))?;
        }
        file.write_all(bytes)
            .map_err(|e| SecretError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| SecretError::Io(e.to_string()))?;
    }
    std::fs::rename(&temp, path).map_err(|e| SecretError::Io(e.to_string()))
}

fn random_bytes<const N: usize>() -> Result<[u8; N], SecretError> {
    // A v4 UUID carries 122 bits of OS entropy, which is plenty for a salt or a
    // nonce, and avoids pulling in a random number crate whose default features
    // we do not control.
    let mut out = [0u8; N];
    let mut filled = 0;
    while filled < N {
        let uuid = uuid::Uuid::new_v4();
        for byte in uuid.as_bytes() {
            if filled == N {
                break;
            }
            out[filled] = *byte;
            filled += 1;
        }
    }
    Ok(out)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
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
        let mut envelope: Envelope =
            serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();
        envelope.m_cost = 8;
        std::fs::write(&temp.0, serde_json::to_vec(&envelope).unwrap()).unwrap();

        let reopened = EncryptedFile::new(&temp.0);
        let error = reopened.unlock("correct horse battery").unwrap_err();
        assert!(matches!(error, SecretError::WrongMasterPassword), "{error}");

        // Flip a byte of the ciphertext.
        let mut envelope: Envelope =
            serde_json::from_slice(&std::fs::read(&temp.0).unwrap()).unwrap();
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
}

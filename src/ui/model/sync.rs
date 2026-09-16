//! 云同步的状态:设置页要显示的那一份(`SyncStatus`)、凭据三级存储
//! (`CredentialStore`)、以及可编辑的一半(`SyncForm`)。
//!
//! 措辞在这里定(三级存储各自怎么说、凭据到底存到哪一级),页面只显示 —— 所以它
//! 和 `parse::sync` 里的 `credentials_label` 是一对,而不是和游戏状态住一起。

use crate::ui::*;

/// One game's sync situation, as reported by `sync.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncGameRow {
    pub id: String,
    pub name: String,
    pub locations: u64,
    /// Set when a save location cannot be resolved right now (unplugged disk,
    /// removed prefix) — better to say so than to fail at sync time.
    pub problem: Option<String>,
    /// Human-readable "when and how it went" for the last sync.
    pub last: Option<String>,
}

impl SyncGameRow {
    /// One line describing the last sync of this game.
    pub(in crate::ui) fn last_label(&self) -> String {
        match &self.last {
            Some(last) => last.clone(),
            None => "还没同步过".to_string(),
        }
    }
}

/// Cloud-sync state for the settings page. Never carries a secret *value*.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncStatus {
    /// The non-secret settings, as stored (`[sync]` in the config).
    pub settings: Value,
    pub remote: String,
    pub rclone: Option<String>,
    /// kopia 可执行文件；没有就是没装（那条路走不通，另一条照常）。
    pub kopia: Option<String>,
    /// 当前生效的引擎：`rclone` | `kopia`。两个引擎在桶里各写各的区域，所以这一条
    /// 必须显示出来 —— 选错了不会报错，只会"看不见对面的存档"。
    pub engine: String,
    /// kopia 仓库在桶里的前缀（"连接信息"要给用户看的几个值之一）。
    pub kopia_prefix: String,
    pub keyring: String,
    pub ephemeral: bool,
    /// `system` | `encrypted-file` | `session-only`.
    pub store_kind: String,
    /// Only meaningful for `encrypted-file`.
    pub store_locked: bool,
    /// `keyring.store.path` —— 明文/加密两种文件模式下就是那个文件的路径
    /// (系统密钥环与内存那一级没有路径,是空串)。
    pub store_path: String,
    /// 主密码凭据文件的路径。三种存储下 daemon 都会报它(`keyring.secrets_file`),
    /// 而 `store_path` 只在文件模式里才有 —— 所以"凭据会存到哪"一律用它。
    pub master_file: String,
    pub min_master_password: usize,
    pub secrets: Vec<String>,
    pub ready: bool,
    pub problem: Option<String>,
    pub games: Vec<SyncGameRow>,
}

impl SyncStatus {
    /// Whether one of our credential slots is filled. `sync.status` reports
    /// account names only — never a value.
    pub(in crate::ui) fn has_secret(&self, account: &str) -> bool {
        self.secrets.iter().any(|a| a == account)
    }

    /// 凭据现在存在哪一级(ADR-014)。
    pub(in crate::ui) fn store(&self) -> CredentialStore {
        CredentialStore::from_wire(&self.store_kind)
    }
}

/// 凭据三级存储里**现在生效**的那一级。
///
/// 这是 UI 最容易说错的一件事:没有密钥环的机器上凭据只在内存里,说成"已存入系统
/// 密钥环"就是在骗用户 —— 他会以为重启之后还在。所以措辞一律从这里取,别在文案里
/// 写死某一级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ui) enum CredentialStore {
    /// 系统密钥环(Secret Service;Windows 上是将来要接的凭据管理器)。
    #[default]
    System,
    /// **默认落点**:明文凭据文件(权限 0600),见 `secrets/plain.rs`。
    Plain,
    /// 可选:主密码加密文件(Argon2id + ChaCha20-Poly1305,见 `secrets/encrypted.rs`)。
    File,
    /// 仅本次会话:本机没有可持久化的后端,守护进程一重启就没了。
    Session,
}

impl CredentialStore {
    /// `sync.status` 里的 `keyring.store.kind`。不认识的答复按最坏情况算:
    /// 当作系统密钥环,不吓唬用户。
    pub(in crate::ui) fn from_wire(kind: &str) -> Self {
        match kind {
            "plain-file" => Self::Plain,
            "encrypted-file" => Self::File,
            "session-only" => Self::Session,
            _ => Self::System,
        }
    }

    /// 页面用它挑要画哪一块(见 `sync.slint` 的 `store-kind`:
    /// 0 密钥环 / 1 加密文件 / 2 内存 / 3 明文文件)。
    pub(in crate::ui) fn index(self) -> i32 {
        match self {
            Self::System => 0,
            Self::File => 1,
            Self::Session => 2,
            Self::Plain => 3,
        }
    }

    /// 用户看到的这一级的名字,能直接接在"存入 / 删除"后面。
    pub(in crate::ui) fn name(self) -> &'static str {
        match self {
            Self::System => "系统密钥环",
            Self::Plain => "明文凭据文件",
            Self::File => "主密码凭据文件",
            Self::Session => "本次会话的内存",
        }
    }

    /// 保存成功后的落点说明。`what` 是"凭据"或"同步密码"。
    ///
    /// 三级的说法必须分开写:"已存入系统密钥环、磁盘上没有明文"这套词只对第一级成立 ——
    /// 文件那一级是加密落盘的,内存那一级则在守护进程重启后就没了。
    pub(in crate::ui) fn saved_note(self, what: &str) -> String {
        match self {
            Self::System => format!("{what}已存入系统密钥环（磁盘上没有明文）"),
            Self::Plain => {
                format!(
                    "{what}已保存到明文凭据文件（权限 0600，只有你能读；想更严可以设主密码加密）"
                )
            }
            Self::File => format!("{what}已加密写入主密码凭据文件（只有主密码能打开它）"),
            Self::Session => format!(
                "{what}只在本次会话的内存里 —— 本机没有可用的密钥环，设一个主密码才能留住它"
            ),
        }
    }

    /// 只有内存可用时,保存被拒绝的理由。
    ///
    /// 内存那一级是**过渡态**(例如命令行"先存凭据、再封进文件"),不能当作落点:
    /// 没有密钥环的机器(含尚未接凭据管理器的 Windows)必须先把主密码设起来,
    /// 否则用户以为存好了,重启后凭据就没了。
    pub(in crate::ui) fn needs_master_password() -> &'static str {
        "本机既没有系统密钥环、凭据文件也写不下去（查一下配置目录的写权限）。\
         在那之前凭据只会留在内存里，守护进程一重启就没了"
    }
}

/// `[sync] engine` 的取值。
///
/// 缺失、认不出来、或者干脆是个空串 —— 一律按 `rclone` 算：这个键是 2026-09-16
/// 才有的，在那之前写下的每一份配置当年级的都是 rclone。
pub(in crate::ui) fn engine_field(settings: &Value) -> String {
    match settings.get("engine").and_then(|v| v.as_str()) {
        Some("kopia") => "kopia".to_string(),
        _ => "rclone".to_string(),
    }
}

/// 换引擎之后**必须说出口**的那句话。
///
/// 两个引擎在桶里各写各的区域：换过去之后，另一个引擎传的版本**不会**出现在列表里
/// —— 数据都还在桶里，只是这边读不出来，而这**不会报错**。daemon 专门回了
/// `engine_changed` 就是为了它（见 `daemon::sync_rpc::rpc_sync_set_settings`）；
/// 从前的 UI 把这个回包整个丢掉了，于是这条警告一次都没显示过。
///
/// 措辞放这里（而不是 `.slint` 里），因为它要被测：两个引擎各自的名字来自
/// [`SyncEngine::label`] 的同一套说法，不能一处写 "rclone(zip)"、另一处写 "rclone"。
pub(in crate::ui) fn engine_switched_note(engine: &str) -> String {
    use crate::config::SyncEngine;
    let (switched_to, unseen) = if engine == "kopia" {
        (SyncEngine::Kopia, SyncEngine::Rclone)
    } else {
        (SyncEngine::Rclone, SyncEngine::Kopia)
    };
    format!(
        "已改用 {}：{} 传上去的版本不会显示在这里（数据还在 bucket 里，只是这边读不出来）",
        switched_to.label(),
        unseen.label()
    )
}

/// The editable half of the sync settings.
#[derive(Debug, Clone, Default)]
pub(in crate::ui) struct SyncForm {
    pub(in crate::ui) loaded: bool,
    /// Set as soon as the user edits a *settings* field. `sync.status` replies
    /// can land seconds after the request (the daemon probes the keyring on the
    /// way), so a reply that was already in flight must never overwrite what
    /// the user is in the middle of typing. Cleared once a save succeeds.
    pub(in crate::ui) settings_dirty: bool,
    pub(in crate::ui) enabled: bool,
    /// 选的引擎：`rclone` | `kopia`。空串按 `rclone` 算（老配置没有这个键）。
    pub(in crate::ui) engine: String,
    pub(in crate::ui) endpoint: String,
    pub(in crate::ui) bucket: String,
    pub(in crate::ui) prefix: String,
    pub(in crate::ui) keep_versions: String,
    pub(in crate::ui) key_id: String,
    pub(in crate::ui) app_key: String,
    /// Master password for the credential file (unlock, or set one up).
    pub(in crate::ui) master_password: String,
    /// kopia 仓库密码。**可以留空** —— 留空就是用默认的 `kotori`。
    pub(in crate::ui) kopia_password: String,
    /// 删除主密码凭据文件前的二次确认(里面的凭据会一起消失)。
    pub(in crate::ui) confirm_master_delete: bool,
    /// kopia 的"连接信息"默认折叠 —— 里面有桶名和密码状态,不该一打开就摊开。
    pub(in crate::ui) connection_revealed: bool,
    pub(in crate::ui) msg: Option<String>,
    pub(in crate::ui) busy: bool,
}

impl SyncForm {
    /// Fill the form from what the daemon reports. Secrets are never echoed, so
    /// their inputs are left alone here: they are only cleared when a save
    /// actually consumed them (`SyncCredentialsSaved`).
    ///
    /// Everything is skipped while `settings_dirty` is set — see the field.
    pub(in crate::ui) fn apply(&mut self, status: &SyncStatus, settings: &Value) {
        self.loaded = true;
        self.confirm_master_delete = false;
        let _ = status;
        if self.settings_dirty {
            return;
        }
        self.enabled = settings["enabled"].as_bool().unwrap_or(false);
        self.engine = engine_field(settings);
        self.endpoint = str_field(settings, "endpoint");
        self.bucket = str_field(settings, "bucket");
        self.prefix = str_field(settings, "prefix");
        self.keep_versions = settings["keep_versions"].as_u64().unwrap_or(0).to_string();
    }

    /// The patch sent to `sync.set_settings`.
    pub(in crate::ui) fn patch(&self) -> Value {
        let keep = self.keep_versions.trim().parse::<u32>().unwrap_or(0);
        serde_json::json!({
            "enabled": self.enabled,
            "engine": if self.engine.trim().is_empty() { "rclone" } else { self.engine.trim() },
            "endpoint": self.endpoint.trim(),
            "bucket": self.bucket.trim(),
            "prefix": self.prefix.trim(),
            "keep_versions": keep,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{sync_payload, sync_status_fixture};

    #[test]
    fn the_sync_form_seeds_from_settings_and_never_from_secrets() {
        let payload = sync_payload();
        let mut form = SyncForm::default();
        form.apply(&sync_status_fixture(), &payload["settings"]);

        assert!(form.loaded);
        assert!(form.enabled);
        assert_eq!(form.endpoint, "");
        assert_eq!(form.bucket, "kotori-saves");
        assert_eq!(form.prefix, "kotori");
        assert_eq!(form.keep_versions, "0");

        // The daemon reports *which* secrets exist, never their values, so the
        // inputs must start empty even though they are stored.
        assert!(form.key_id.is_empty());
        assert!(form.app_key.is_empty());

        // The patch mirrors the form, trimmed.
        form.bucket = "  spaced  ".into();
        let patch = form.patch();
        assert_eq!(patch["bucket"], "spaced");
        assert_eq!(patch["enabled"], true);
        assert!(
            patch.get("key_id").is_none() && patch.get("force").is_none(),
            "settings patches must carry no secrets: {patch}"
        );
    }

    /// 换引擎那句话必须点名**另一个**引擎 —— 用户要知道自己"看不见"的是什么。
    ///
    /// 两个名字都取自 `SyncEngine::label`,和 daemon、环境检查页用的是同一套说法:
    /// 一处写 "rclone(zip)"、另一处写 "rclone",用户就没法把两句话对上。
    #[test]
    fn switching_the_engine_names_the_one_that_becomes_invisible() {
        let to_kopia = engine_switched_note("kopia");
        assert!(to_kopia.contains("已改用 kopia"), "{to_kopia}");
        assert!(to_kopia.contains("rclone(zip)"), "{to_kopia}");
        assert!(to_kopia.contains("不会显示"), "{to_kopia}");

        let to_rclone = engine_switched_note("rclone");
        assert!(to_rclone.contains("已改用 rclone(zip)"), "{to_rclone}");
        assert!(to_rclone.contains("kopia"), "{to_rclone}");

        // 认不出来的值按 rclone 算 —— 与 `engine_field` 同一条规矩（老配置没有这个键）,
        // 不能在这里玩出第三种说法。
        assert_eq!(engine_switched_note("???"), to_rclone);
    }
}

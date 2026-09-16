//! 云同步那一段配置：用哪个引擎、连哪个桶、留多少版。
//!
//! 单独成文件，是因为这里只回答"设置长什么样"：能不能开始（`sync::validate`）、
//! 东西放在哪（`sync::remote_paths`）、凭据从哪来（`crate::secrets`）全在别处。
//!
//! **这里不存任何秘密。** B2 的 key 和 kopia 的仓库密码都在操作系统的凭据库里
//! （见 [`crate::secrets`]），落盘时是加密的，用户还能用标准工具读回来。这个结构
//! 只装非敏感设置（ADR-010）。

use serde::{Deserialize, Serialize};

/// 用哪个引擎把存档送上云。
///
/// **全局一个，不是每个游戏一个**：两个引擎在桶里的布局互不相通，同一个桶里混着
/// 用只会让"有些存档看不见"变成一件要靠猜的事。换机器（双系统）也必须选同一个。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncEngine {
    /// rclone + 一版一个 zip。谁都能用别的工具把包拿下来解开（不加密）。
    #[default]
    Rclone,
    /// kopia 仓库：内容寻址、去重、自带加密（密码默认 `kotori`，见 ADR-010 的修正）。
    Kopia,
}

impl SyncEngine {
    /// 给人看的名字，UI 与环境检查页共用一处，免得两边说法不一致。
    pub fn label(self) -> &'static str {
        match self {
            SyncEngine::Rclone => "rclone(zip)",
            SyncEngine::Kopia => "kopia",
        }
    }
}

/// Cloud save sync settings.
///
/// **Nothing secret is stored here.** The B2 credentials and the kopia repository
/// password live in the OS keyring (see [`crate::secrets`]), which encrypts them
/// at rest while still letting their owner read them back with standard tooling.
/// This struct only holds the non-sensitive settings (ADR-010).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 用哪个引擎。旧配置没有这个键，serde 默认成 [`SyncEngine::Rclone`]——
    /// 那正是它们当年写下的东西。
    #[serde(default)]
    pub engine: SyncEngine,
    /// Optional override for the storage API endpoint.
    ///
    /// Normally **empty**: rclone's native `b2` backend discovers the right
    /// regional API host from the credentials itself, and every B2 account
    /// works that way. Set it only to pin a specific endpoint, as a full URL
    /// (`https://api001.backblazeb2.com`) — a bare host does not work.
    ///
    /// Note this is *not* the `s3.<region>.backblazeb2.com` value the B2
    /// console shows: that is the S3-compatible API, a different service this
    /// backend does not speak. [`crate::sync::validate`] rejects it by name
    /// rather than letting rclone fail with a confusing 404.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub bucket: String,
    /// Folder inside the bucket that kotori owns.
    #[serde(default = "default_sync_prefix")]
    pub prefix: String,
    /// Version packages kept per game; `0` keeps all of them, which is the
    /// default — silently dropping an old save is worse than using space.
    #[serde(default)]
    pub keep_versions: u32,
    /// rclone 可执行文件在哪。**留空 = 由 kotori 自己找**（`KOTORI_RCLONE` →
    /// kotori 自己的目录 → `PATH`，顺序与理由见 [`crate::sync::executables`]）。
    ///
    /// 填**目录**或**完整文件路径**都行。这一项是给"不想配环境变量的人"准备的
    /// （用户 2026-09-16）：Windows 上把 kopia/rclone 放在一个目录里、PATH 里什么都
    /// 不加才是常态，而 PATH 这件事普通人根本不会配；从资源管理器复制过来的又多半
    /// 是目录，所以两种都认。
    #[serde(default)]
    pub rclone_binary: String,
    /// kopia 可执行文件在哪；语义与 [`SyncConfig::rclone_binary`] 完全一样。
    #[serde(default)]
    pub kopia_binary: String,
}

fn default_sync_prefix() -> String {
    "kotori".to_string()
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: SyncEngine::default(),
            endpoint: String::new(),
            bucket: String::new(),
            prefix: default_sync_prefix(),
            keep_versions: 0,
            rclone_binary: String::new(),
            kopia_binary: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_config_that_still_carries_encryption_still_loads() {
        // 加密随 crypt 层一起没了（2026-09-16：rclone 这条路一版一个 zip，
        // 包里就是明文，要加密用 kopia）。磁盘上每一份旧配置都还写着这个键 ——
        // 加载必须照常，而下一次写回不能再带上它。
        let toml = r#"
enabled = true
bucket = "kotori-saves"
prefix = "kotori"
encryption = true
keep_versions = 3
"#;
        let config: SyncConfig = toml::from_str(toml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.bucket, "kotori-saves");
        assert_eq!(config.keep_versions, 3);

        let written = toml::to_string(&config).unwrap();
        assert!(!written.contains("encryption"), "{written}");
    }

    #[test]
    fn a_sync_config_without_an_engine_key_is_rclone() {
        // 引擎字段是 2026-09-16 加的。在那之前写下的每一份配置都没有这个键，
        // 而它们当年级的就是 rclone —— 默认值必须是它，不能是别的。
        let config: SyncConfig = toml::from_str("enabled = true\n").unwrap();
        assert_eq!(config.engine, SyncEngine::Rclone);

        let written = toml::to_string(&config).unwrap();
        assert!(written.contains("engine = \"rclone\""), "{written}");
    }

    #[test]
    fn the_engine_survives_a_round_trip() {
        let config = SyncConfig {
            engine: SyncEngine::Kopia,
            ..SyncConfig::default()
        };
        let written = toml::to_string(&config).unwrap();
        let back: SyncConfig = toml::from_str(&written).unwrap();
        assert_eq!(back.engine, SyncEngine::Kopia);
    }

    /// 两个"程序在哪"的键是 2026-09-16 加的：在那之前写下的配置没有它们，
    /// 加载必须照常，而且默认是**空**（＝由 kotori 自己找，见 `sync::executables`）。
    #[test]
    fn a_sync_config_without_binary_paths_finds_the_programs_itself() {
        let config: SyncConfig = toml::from_str("enabled = true\n").unwrap();
        assert!(config.rclone_binary.is_empty());
        assert!(config.kopia_binary.is_empty());

        // 填了就存得住 —— 这是"设置页里指路"整件事的前提。
        let filled = SyncConfig {
            kopia_binary: r"D:\tools\kopia".to_string(),
            ..SyncConfig::default()
        };
        let back: SyncConfig = toml::from_str(&toml::to_string(&filled).unwrap()).unwrap();
        assert_eq!(back.kopia_binary, r"D:\tools\kopia");
    }
}

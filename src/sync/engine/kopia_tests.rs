//! `kopia` 的单元测试：失败诊断的分类、连接身份。
//!
//! 从 `kopia.rs` 拆出来 —— 理由同上，500 行是硬线（AGENTS.md）。

use super::*;

#[test]
fn kopia_failures_are_explained_and_deprecation_noise_is_dropped() {
    // 实测(2026-09-19,真机 B2):弃用警告 + bucket not found 混在一段 stderr
    // 里直接透传,让"改了桶名"看起来像"没生效"。
    let stderr = "WARNING The b2 backend is deprecated and will be removed in a future release\n\
                      ERROR can't connect to storage: bucket not found\n";
    let explained = super::super::diagnostics::explain_kopia_failure(stderr);
    assert!(explained.contains("核对桶名"), "{explained}");
    assert!(
        explained.contains("bucket not found"),
        "原话保留: {explained}"
    );
    assert!(
        !explained.contains("deprecated"),
        "弃用警告是噪音: {explained}"
    );

    // 密码错给的是密码那条指引。
    let explained = super::super::diagnostics::explain_kopia_failure(
        "ERROR failed to connect to repository: invalid password",
    );
    assert!(explained.contains("仓库密码"), "{explained}");
}

#[test]
fn the_default_password_is_the_one_every_machine_shares() {
    // 所有端一致才有"双系统互通"这一说；改它等于把所有老仓库锁在门外。
    assert_eq!(DEFAULT_PASSWORD, "kotori");
}

/// 连接身份里必须带着 endpoint（BUG-21）：换过地址之后，旧的 `repository.config`
/// 指向的是**另一个目标**，直接复用会让用户看到"地址明明改了、同步还是老样子"。
#[test]
fn a_changed_endpoint_is_a_different_connection() {
    let engine = |endpoint: &str| {
        Kopia::with_binary(
            PathBuf::from("/bin/true"),
            SyncConfig {
                endpoint: endpoint.to_string(),
                bucket: "bucket".to_string(),
                ..SyncConfig::default()
            },
            Keyring::memory(),
        )
        .with_home(std::env::temp_dir().join("kotori-kopia-target"))
    };

    let plain = engine("").current_target();
    let b2 = engine("https://api001.backblazeb2.com").current_target();
    assert_ne!(plain, b2, "换了 endpoint 就是另一个连接目标");
    assert_eq!(
        engine("https://api001.backblazeb2.com/").current_target(),
        b2,
        "尾斜杠不该算成两个地址"
    );
}

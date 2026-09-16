//! 测试夹具：跨模块共用的同步配置。
//!
//! 校验、远端路径、rclone 环境三处的测试都要同一个"普通 B2 设置"，放在这里
//! 免得各写一份；本文件只在 `cfg(test)` 下编译。

use crate::config::SyncConfig;

pub(super) fn settings() -> SyncConfig {
    SyncConfig {
        enabled: true,
        endpoint: String::new(),
        bucket: "kotori-saves".to_string(),
        prefix: "prefix".to_string(),
        keep_versions: 0,
    }
}

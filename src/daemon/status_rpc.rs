//! The requests about the daemon's own state rather than about a game: its
//! status, the wine prefix it would use, reloading the config from disk,以及
//! **配置放在哪儿**(设置页那只"切到便携 / 切到默认"的按钮)。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

/// 这台机器上"配置能从哪儿来"的两个地点。
///
/// 用户 2026-09-19 定的规矩:只有这两个,不做自定义路径(见 `config::config_path`)。
/// 便携安装把 `config.toml` 放在 `kotori.exe` 旁边,配置就跟着程序走;否则落在平台
/// 默认目录。启动时**便携优先**,所以两处都有时用户可能分不清哪份在生效 —— 设置页
/// 那只按钮就是把这件事摆到明面上、并且允许换。
pub(super) struct ConfigSources {
    /// 二进制同目录那个 `config.toml`;取不出自己的位置时是 `None`。
    portable: Option<PathBuf>,
    default: PathBuf,
}

impl ConfigSources {
    /// 真机:按二进制位置与平台默认目录探测。
    pub(super) fn detect() -> Self {
        Self {
            portable: crate::config::portable_config_path(),
            default: crate::config::default_config_path(),
        }
    }

    /// 显式给两个地点(测试注入临时目录用)。
    #[cfg(test)]
    pub(super) fn at(portable: Option<PathBuf>, default: PathBuf) -> Self {
        Self { portable, default }
    }

    /// 切到其中一个地点:得到目标路径,或者一句"为什么不行"。
    pub(super) fn target(&self, portable: bool) -> Result<PathBuf, String> {
        if crate::config::config_path_is_pinned() {
            return Err(
                "这台机器的配置路径由环境变量 KOTORI_CONFIG 指定，切换来源不会生效".to_string(),
            );
        }
        if portable {
            self.portable
                .clone()
                .ok_or_else(|| "取不到 kotori 自己的位置，没法把配置放到它旁边".to_string())
        } else {
            Ok(self.default.clone())
        }
    }
}

impl Daemon {
    pub(super) async fn rpc_status(&self) -> Result<Value, String> {
        let games = self.config.read().await.games.len();
        let sessions = self.engine.list_sessions().await;
        Ok(json!({
            "running": true,
            "games": games,
            // 实际生效的那份配置(二进制同目录优先,见 `config::config_path`),
            // `kotori status` 打印整个回包,用户由此知道配置与日志在哪。
            "config_path": self.config_path.read().await.display().to_string(),
            // 设置页据此算出"现在用的是哪一份"并把按钮摆对。
            "config_portable_path": self.config_sources.portable.as_ref().map(|p| p.display().to_string()),
            "config_pinned": crate::config::config_path_is_pinned(),
            "sessions": sessions
                .iter()
                .map(|s| json!({
                    "session_id": s.session_id,
                    "game_id": s.game_id,
                    "gamescope_pid": s.gamescope_pid,
                    "process_name": s.process_name,
                    "watch_only": s.watch_only,
                    "elapsed_secs": s.started_at.elapsed().as_secs(),
                }))
                .collect::<Vec<_>>(),
        }))
    }

    /// 「切到便携配置 / 切到默认配置」。
    ///
    /// 内存里那份配置**不动**(内容本来就是同一份),变的只有"以后存到哪":新地点先
    /// 落一份当前内容,再把被留下的便携那份改名让路(切到默认目录时必须 —— 别的
    /// 都白搭,启动时便携优先)。**不需要重启**:daemon 是唯一写者,它记住的就是新
    /// 路径(见 `Daemon::config_path`)。
    pub(super) async fn rpc_config_set_source(&self, portable: bool) -> Result<Value, String> {
        let target = self.config_sources.target(portable)?;
        let current = self.config_path.read().await.clone();
        if current == target {
            return Ok(json!({
                "config_path": target.display().to_string(),
                "changed": false,
            }));
        }

        let config = self.config.read().await.clone();
        // 离开便携地点时,那份旧文件必须让路(改名,不删)。
        let disable_current = !portable;
        let moved = crate::config::relocate_config(&current, &target, &config, disable_current)
            .map_err(|e| format!("切换配置来源失败: {e}"))?;
        *self.config_path.write().await = target.clone();

        tracing::info!(
            "配置来源已切换: {} → {}{}",
            current.display(),
            target.display(),
            moved
                .as_ref()
                .map(|path| format!("(旧文件改名为 {})", path.display()))
                .unwrap_or_default()
        );
        Ok(json!({
            "config_path": target.display().to_string(),
            "changed": true,
            "moved": moved.map(|path| path.display().to_string()),
            "portable": portable,
        }))
    }

    /// Which wine prefix would be used, and what was found on this machine.
    /// `env.report`:设置页那一组「环境检查」(用户 2026-09-13:检查只在 GUI 里)。
    ///
    /// 用户点开设置页时问一次 —— 探测会真去跑外部程序(`--version`、portal 代理、
    /// 密钥环问一次),不适合跟 `daemon.status` 一起每 3 秒轮询。
    pub(super) async fn rpc_env_report(&self) -> Result<Value, String> {
        // 拿一份快照就够了：探测要跑好几秒，别一直占着配置的读锁。
        let settings = self.config.read().await.sync.clone();
        let report = crate::platform::report(&settings).await;
        serde_json::to_value(&report).map_err(|e| e.to_string())
    }

    pub(super) async fn rpc_wine_status(&self) -> Result<Value, String> {
        let config = self.config.read().await;
        let detected = crate::wine::detect_prefixes(Path::new(""));
        Ok(json!({
            "configured": config.wine.prefix,
            "default": crate::wine::default_prefix(),
            "environment": std::env::var("WINEPREFIX").ok(),
            "detected": detected,
        }))
    }

    /// Set (or clear, with `null`) the machine-wide wine prefix.
    pub(super) async fn rpc_set_wine_prefix(&self, value: Value) -> Result<Value, String> {
        let prefix: Option<PathBuf> = match value {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => None,
            Value::String(text) => {
                let path = PathBuf::from(text.trim());
                // Accept a prefix that exists (must look like one) or a path
                // that does not exist yet (wine will populate it), but reject a
                // directory that clearly is not a prefix.
                if path.exists() && !path.join("drive_c").is_dir() {
                    return Err(format!(
                        "这不是一个 wine prefix（缺少 drive_c）: {}",
                        path.display()
                    ));
                }
                Some(path)
            }
            _ => return Err("prefix 必须是路径字符串或 null".to_string()),
        };

        self.mutate_config(|config| {
            config.wine.prefix = prefix.clone();
            tracing::info!("wine prefix set to {:?}", prefix);
            Ok(json!({ "prefix": prefix }))
        })
        .await
    }

    pub(super) async fn rpc_reload_config(&self) -> Result<Value, String> {
        let path = self.config_path.read().await.clone();
        let new_config = crate::config::load_at(&path).map_err(|e| e.to_string())?;
        *self.config.write().await = new_config;
        tracing::info!("configuration reloaded");
        Ok(json!({ "success": true }))
    }
}

//! rclone 引擎：一版一个 zip 包，整份上、整份下。
//!
//! 桶里的布局是 `<bucket>/<prefix>/games/<游戏id>/<stamp>.zip`，"最新的一版"就是
//! 名字最大的那个包——不额外维护指针文件，少一个会写坏的东西。
//!
//! 三条规则（ADR-010 / ADR-012）：
//!   * **凭据只走环境变量**，绝不进命令行、绝不落盘（见 [`super::super::rclone_env`]）；
//!   * 一次传输就是一个对象的一来一回，不存在"目录对目录地合并"；
//!   * 失败要说清失败在哪（[`explain_failure`] 把 rclone 的 stderr 翻成人话）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};

use super::super::archive::{self, Manifest, PackReport};
use super::super::save_targets::SaveTarget;
use super::super::{
    SyncError, copyto_args, deletefile_args, game_remote, list_files_args, package_remote,
    parse_packages, rclone_env, remote_root,
};
use super::diagnostics::explain_failure;

/// rclone + 一版一个 zip。
pub(super) struct RcloneZip {
    binary: PathBuf,
    settings: SyncConfig,
    keyring: Keyring,
}

impl RcloneZip {
    /// 找到 rclone，找不到就当场说清楚。
    pub(super) fn new(settings: SyncConfig, keyring: Keyring) -> Result<Self, SyncError> {
        let binary = crate::sync::find_rclone(&settings.rclone_binary).ok_or_else(|| {
            SyncError::EngineMissing {
                engine: "rclone",
                detail: "找不到 rclone：在设置页填上它的位置，或者 Arch: sudo pacman -S rclone"
                    .to_string(),
            }
        })?;
        Ok(Self {
            binary,
            settings,
            keyring,
        })
    }

    /// 钉在一个指定的二进制上。测试用（也留给把 rclone 放在怪地方的场景——
    /// `KOTORI_RCLONE` 由 [`crate::sync::find_rclone`] 处理）。
    #[cfg(test)]
    pub(super) fn with_binary(binary: PathBuf, settings: SyncConfig, keyring: Keyring) -> Self {
        Self {
            binary,
            settings,
            keyring,
        }
    }

    pub(super) fn describe(&self) -> String {
        format!("rclone ({})", self.binary.display())
    }

    /// 凭据，按子进程该看到的样子。
    ///
    /// 两个 B2 值是这个引擎仅有的秘密：没有同步密码、没有 crypt 层。
    fn env(&self) -> Result<Vec<(String, String)>, SyncError> {
        let read = |key: SecretKey| {
            self.keyring
                .get(key)
                .map_err(|e| SyncError::Command(e.to_string()))
        };
        let missing = |what: &str| SyncError::Config(format!("密钥环里没有{what}"));

        let key_id = read(SecretKey::B2KeyId)?.ok_or_else(|| missing("B2 key id"))?;
        let app_key = read(SecretKey::B2AppKey)?.ok_or_else(|| missing("B2 application key"))?;
        Ok(rclone_env(&self.settings, &key_id, &app_key))
    }

    /// 唯一一处 spawn rclone 的地方。
    async fn run(&self, args: &[String], timeout: Duration) -> Result<String, SyncError> {
        let env = self.env()?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .envs(env)
            // 连"没有凭据"那条路也必须忽略用户自己的 rclone.conf。
            .env("RCLONE_CONFIG", super::super::null_config_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 超时的传输不许在后台继续跑。
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.binary.display())))?;

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                SyncError::Command(format!("rclone {} 超过 {:?} 未完成", args[0], timeout))
            })?
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.binary.display())))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                format!("退出码 {:?}", output.status.code())
            } else {
                explain_failure(&stderr)
            };
            return Err(SyncError::Command(format!(
                "rclone {} 失败: {detail}",
                args[0]
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// `mkdir` 前缀目录是最便宜的一步，同时验到凭据、桶和写权限，而且幂等。
    pub(super) async fn check(&self) -> Result<String, SyncError> {
        let root = remote_root(&self.settings);
        self.run(&["mkdir".to_string(), root.clone()], COMMAND_TIMEOUT)
            .await?;
        Ok(root)
    }

    /// 这个游戏在云端有哪些版本，最旧在前。
    pub(super) async fn versions(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        let remote = game_remote(&self.settings, game_id);
        let output = self.run(&list_files_args(&remote), COMMAND_TIMEOUT).await?;
        // 只报名字长得像我们自己的包的那些。
        Ok(parse_packages(&output))
    }

    pub(super) async fn send(
        &self,
        game_id: &str,
        stamp: &str,
        targets: &[SaveTarget],
        work_dir: &Path,
        timeout: Duration,
    ) -> Result<PackReport, SyncError> {
        let zip = work_dir.join(format!("{stamp}{}", super::super::PACKAGE_SUFFIX));
        let report = archive::pack(&zip, targets, chrono::Utc::now())
            .map_err(|e| SyncError::Command(format!("打包失败: {e}")))?;

        // 本机一个存档目录都没有：上传一个空包只会往版本列表里塞垃圾。
        // 两个引擎共用这条判据（见 `Backend::send` 的契约）。
        if report.locations.is_empty() {
            return Ok(report);
        }

        let remote = package_remote(&self.settings, game_id, stamp);
        let args = copyto_args(&zip.to_string_lossy(), &remote);
        self.run(&args, timeout).await?;
        Ok(report)
    }

    pub(super) async fn fetch(
        &self,
        game_id: &str,
        stamp: &str,
        into: &Path,
        timeout: Duration,
    ) -> Result<Manifest, SyncError> {
        std::fs::create_dir_all(into)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", into.display())))?;
        let zip = into.join("package.zip");
        let remote = package_remote(&self.settings, game_id, stamp);
        let args = copyto_args(&remote, &zip.to_string_lossy());
        self.run(&args, timeout).await?;
        archive::extract(&zip, into)
            .map_err(|e| SyncError::Command(format!("云端存档 {stamp} 读不出来: {e}")))
    }

    pub(super) async fn remove(&self, game_id: &str, stamp: &str) -> Result<(), SyncError> {
        let remote = package_remote(&self.settings, game_id, stamp);
        let args = deletefile_args(&remote);
        self.run(&args, COMMAND_TIMEOUT).await.map(|_| ())
    }
}

/// 一次 rclone 调用的时间上限。
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

//! kopia 引擎：内容寻址的仓库，一版一个快照。
//!
//! ## 一版到底是什么
//!
//! kopia 快照的是**目录树**，而一个游戏的存档位置散在好几个地方——`snapshot
//! create` 收到多个路径会拍出**多个快照**（每个源一个 ID，实测），那样"回到某一
//! 时刻"就得靠一堆 ID 拼。所以这一版先按 zip 那条路的老规矩摆成一个目录
//! （[`archive::materialize`]：`<key>/<相对路径>` 加一份 `kotori-manifest.json`），
//! 再对它拍**一次**快照。于是：
//!
//!   * 一个游戏一个版本 = 一个快照，回退是"恢复那一个"，不是"拼好几个"；
//!   * 恢复出来的目录结构与 zip 里的一模一样，`archive::plan` 那套合并判定原样
//!     复用——ADR-012 的"只取新的""本地独有文件不删"在 kopia 下也一样成立。
//!
//! ## 两个坑（都实测过）
//!
//!   * `snapshot delete` **默认只演练**，必须带 `--delete`（见 `kopia_args`）；
//!   * kopia 默认往 `~/.cache/kopia` 写日志，那个目录不存在时每一步都吐一行
//!     `write error: unable to open log file`。所以 `KOPIA_LOG_DIR` 必须指到我们
//!     自己的数据目录下（沙箱、干净账户、CI 里都会遇到）。
//!
//! ## 连接状态
//!
//! `KOPIA_CONFIG_PATH` 指向的那份配置是"连上了"的唯一凭据。它不在时先 `connect`
//! 再 `create`（桶里可能已经有仓库，也可能没有）；它在了就直接用。桶/prefix 换了
//! 要重连，所以那份配置旁边还记着当时连的是哪儿（[`Kopia::target_marker`]）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::SyncConfig;
use crate::secrets::{Keyring, SecretKey};

use super::super::SyncError;
use super::super::archive::{self, Manifest, PackReport};
use super::super::save_targets::SaveTarget;
use super::kopia_args as args;

/// 默认仓库密码。**所有端一致**，这样双系统/多机互通，而且"自己下载 kopia
/// 读"的人知道该试什么。用户可以在设置页自己设一个更强的。
pub(super) const DEFAULT_PASSWORD: &str = "kotori";

/// 一次 kopia 调用的时间上限。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// kopia 0.22 的仓库与快照。
pub(super) struct Kopia {
    binary: PathBuf,
    settings: SyncConfig,
    keyring: Keyring,
    /// 数据目录下属于 kopia 的一切。**不放存档目录旁边**，也不进 git。
    home: PathBuf,
    /// 仓库放在一个本地目录（`KOTORI_KOPIA_REPOSITORY`）而不是 B2。
    /// 测试与"仓库放 NAS 上"这两种情况共用它。
    local_repository: Option<PathBuf>,
}

impl Kopia {
    pub(super) fn new(settings: SyncConfig, keyring: Keyring) -> Result<Self, SyncError> {
        // 设置页可以指点位置（目录或完整路径），`KOTORI_KOPIA` 与 PATH 是它后面的
        // 两步 —— 顺序与理由见 [`crate::sync::executables`]。
        let binary =
            crate::sync::find_kopia(&settings.kopia_binary).ok_or_else(|| {
                SyncError::EngineMissing {
                    engine: "kopia",
                    detail: "找不到 kopia（≥0.22）：在设置页填上它的位置，或者 Arch: sudo pacman -S archlinuxcn/kopia"
                        .to_string(),
                }
            })?;
        let home = crate::config::data_dir().join("kopia");
        Ok(Self {
            binary,
            settings,
            keyring,
            home,
            local_repository: local_repository_from_env(),
        })
    }

    #[cfg(test)]
    pub(super) fn with_binary(binary: PathBuf, settings: SyncConfig, keyring: Keyring) -> Self {
        Self {
            binary,
            settings,
            keyring,
            home: crate::config::data_dir().join("kopia"),
            local_repository: local_repository_from_env(),
        }
    }

    /// 把 kopia 的家挪到别处。只给测试用：一次测试绝不该碰真实数据目录。
    #[cfg(test)]
    pub(super) fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    /// 把仓库钉在一个本地目录上。只给测试用（生产路径走 `KOTORI_KOPIA_REPOSITORY`）。
    #[cfg(test)]
    pub(super) fn with_local_repository(mut self, path: impl Into<PathBuf>) -> Self {
        self.local_repository = Some(path.into());
        self
    }

    pub(super) fn describe(&self) -> String {
        format!("kopia ({})", self.binary.display())
    }

    fn config_path(&self) -> PathBuf {
        self.home.join("repository.config")
    }

    /// 记着这份配置连的是哪个桶——设置改了就得重连，否则会拿旧桶当新桶用。
    fn target_marker(&self) -> PathBuf {
        self.home.join("target.txt")
    }

    /// `KOTORI_KOPIA_REPOSITORY`：把仓库放到一个本地目录（NAS、挂在别处的盘），
    /// 而不是 B2。见 `kopia_args` 里那段说明——布局与 B2 完全一致，只是仓库在哪不同。
    fn local_repository(&self) -> Option<PathBuf> {
        self.local_repository.clone()
    }

    fn current_target(&self) -> String {
        match self.local_repository() {
            Some(path) => format!("filesystem:{}", path.display()),
            None => format!(
                "{}/{}",
                self.settings.bucket.trim().trim_matches('/'),
                args::repo_prefix(&self.settings)
            ),
        }
    }

    /// 子进程该看到的环境。
    ///
    /// 密码走环境变量，**绝不进 argv**。`KOPIA_USE_KEYRING=false` 是刻意的：
    /// 让 kopia 自己去翻 gnome-keyring 会在没有桌面会话的地方（CI、SSH）报一堆
    /// 用不上的错，而密码本来就在我们自己的凭据库里。
    fn env(&self) -> Result<Vec<(String, String)>, SyncError> {
        let password = self
            .keyring
            .get(SecretKey::KopiaPassword)
            .map_err(|e| SyncError::Command(e.to_string()))?
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_PASSWORD.to_string());

        Ok(vec![
            (
                "KOPIA_CONFIG_PATH".to_string(),
                self.config_path().to_string_lossy().to_string(),
            ),
            (
                "KOPIA_CACHE_DIRECTORY".to_string(),
                self.home.join("cache").to_string_lossy().to_string(),
            ),
            (
                "KOPIA_LOG_DIR".to_string(),
                self.home.join("logs").to_string_lossy().to_string(),
            ),
            ("KOPIA_PASSWORD".to_string(), password),
            ("KOPIA_USE_KEYRING".to_string(), "false".to_string()),
            ("KOPIA_CHECK_FOR_UPDATES".to_string(), "false".to_string()),
        ])
    }

    /// B2 的连接凭据。**只能进 argv**，见 `kopia_args` 的说明。
    fn credentials(&self) -> Result<(String, String), SyncError> {
        let read = |key: SecretKey| {
            self.keyring
                .get(key)
                .map_err(|e| SyncError::Command(e.to_string()))
        };
        let missing = |what: &str| SyncError::Config(format!("密钥环里没有{what}"));
        let key_id = read(SecretKey::B2KeyId)?.ok_or_else(|| missing("B2 key id"))?;
        let key = read(SecretKey::B2AppKey)?.ok_or_else(|| missing("B2 application key"))?;
        Ok((key_id, key))
    }

    fn prepare_dirs(&self) -> Result<(), SyncError> {
        for dir in [
            self.home.clone(),
            self.home.join("cache"),
            self.home.join("logs"),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", dir.display())))?;
        }
        Ok(())
    }

    async fn run(&self, argv: &[String], timeout: Duration) -> Result<String, SyncError> {
        self.prepare_dirs()?;
        let env = self.env()?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(argv)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.binary.display())))?;

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                SyncError::Command(format!("kopia {} 超过 {:?} 未完成", argv[1], timeout))
            })?
            .map_err(|e| SyncError::Command(format!("无法执行 {}: {e}", self.binary.display())))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                format!("退出码 {:?}", output.status.code())
            } else {
                clean_stderr(&stderr)
            };
            return Err(SyncError::Command(format!(
                "kopia {} 失败: {detail}",
                argv[1]
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// 需要"已经连上仓库"的每一步都先过这里。
    async fn ensure_connected(&self) -> Result<(), SyncError> {
        let marker = self.target_marker();
        let target = self.current_target();
        let connected = self.config_path().exists()
            && std::fs::read_to_string(&marker)
                .map(|seen| seen.trim() == target)
                .unwrap_or(false);
        if connected {
            return Ok(());
        }

        // 本地目录仓库不需要 B2 凭据；B2 仓库要（而且那两个值只能进 argv，
        // 见 `kopia_args` 的说明）。
        let (connect, create) = match self.local_repository() {
            Some(path) => {
                let path = path.to_string_lossy().to_string();
                (
                    args::connect_filesystem_args(&path),
                    args::create_filesystem_args(&path),
                )
            }
            None => {
                let (key_id, key) = self.credentials()?;
                (
                    args::connect_args(&self.settings, &key_id, &key),
                    args::create_args(&self.settings, &key_id, &key),
                )
            }
        };

        // 先连：桶里已经有仓库（换机器、或者配置被删了）时就该连，而不是建。
        let connect_error = match self.run(&connect, COMMAND_TIMEOUT).await {
            Ok(_) => {
                self.remember_target(&target)?;
                return Ok(());
            }
            Err(error) => error,
        };
        // 桶里还没有仓库：建一个。**建也失败就报"连"的那个错**——桶里没有仓库时
        // create 才是对的，而它若还是失败（凭据错、桶不存在），create 的报错通常
        // 是"already exists"之类的误导。
        match self.run(&create, COMMAND_TIMEOUT).await {
            Ok(_) => {
                self.remember_target(&target)?;
                Ok(())
            }
            Err(_) => Err(connect_error),
        }
    }

    fn remember_target(&self, target: &str) -> Result<(), SyncError> {
        self.prepare_dirs()?;
        std::fs::write(self.target_marker(), target)
            .map_err(|e| SyncError::Command(format!("无法记下 kopia 连接目标: {e}")))
    }

    /// 连上仓库并列出快照——凭据、桶、仓库密码、读权限一次验完。
    pub(super) async fn check(&self) -> Result<String, SyncError> {
        // 强制作废旧的连接记录：用户可能刚改过桶或 prefix。
        let _ = std::fs::remove_file(self.target_marker());
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_args("kotori-check"), COMMAND_TIMEOUT)
            .await?;
        args::parse_snapshots(&listed).map_err(SyncError::Command)?;
        Ok(match self.local_repository() {
            Some(path) => format!("本地仓库 {}", path.display()),
            None => format!(
                "仓库 {}（前缀 {}）",
                self.settings.bucket.trim(),
                args::repo_prefix(&self.settings)
            ),
        })
    }

    async fn snapshots(&self, game_id: &str) -> Result<Vec<args::Snapshot>, SyncError> {
        self.ensure_connected().await?;
        let listed = self
            .run(&args::snapshot_list_args(game_id), COMMAND_TIMEOUT)
            .await?;
        args::parse_snapshots(&listed).map_err(SyncError::Command)
    }

    /// 我们自己的版本名，最旧在前。
    pub(super) async fn versions(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        Ok(self
            .snapshots(game_id)
            .await?
            .into_iter()
            .map(|snapshot| snapshot.description)
            .collect())
    }

    pub(super) async fn send(
        &self,
        game_id: &str,
        stamp: &str,
        targets: &[SaveTarget],
        work_dir: &Path,
        timeout: Duration,
    ) -> Result<PackReport, SyncError> {
        let payload = work_dir.join("payload");
        std::fs::create_dir_all(&payload)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", payload.display())))?;
        let report = archive::materialize(&payload, targets, chrono::Utc::now())
            .map_err(|e| SyncError::Command(format!("打包失败: {e}")))?;

        // 与 rclone 那条路同一条判据：本机一个存档目录都没有就别往上送。
        if report.locations.is_empty() {
            return Ok(report);
        }

        self.ensure_connected().await?;
        self.run(
            &args::snapshot_create_args(game_id, stamp, &payload.to_string_lossy()),
            timeout,
        )
        .await?;
        Ok(report)
    }

    pub(super) async fn fetch(
        &self,
        game_id: &str,
        stamp: &str,
        into: &Path,
        timeout: Duration,
    ) -> Result<Manifest, SyncError> {
        let snapshots = self.snapshots(game_id).await?;
        let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.description == stamp)
        else {
            return Err(SyncError::Command(format!("kopia 仓库里没有版本 {stamp}")));
        };

        std::fs::create_dir_all(into)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", into.display())))?;
        self.run(
            &args::restore_args(&snapshot.id, &into.to_string_lossy()),
            timeout,
        )
        .await?;
        archive::read_dir_manifest(into)
            .map_err(|e| SyncError::Command(format!("云端存档 {stamp} 读不出来: {e}")))
    }

    pub(super) async fn remove(&self, game_id: &str, stamp: &str) -> Result<(), SyncError> {
        let snapshots = self.snapshots(game_id).await?;
        let Some(snapshot) = snapshots
            .iter()
            .find(|snapshot| snapshot.description == stamp)
        else {
            // 已经不在了：保留窗口是 best-effort，不需要为此报错。
            return Ok(());
        };
        self.run(&args::snapshot_delete_args(&snapshot.id), COMMAND_TIMEOUT)
            .await
            .map(|_| ())
    }
}

/// `KOTORI_KOPIA_REPOSITORY`：本地目录仓库的注入点。
fn local_repository_from_env() -> Option<PathBuf> {
    std::env::var_os("KOTORI_KOPIA_REPOSITORY")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// kopia 的 stderr 收缩成一行可读的话。
///
/// 它爱在前面写时间戳、在后面追加一堆 `write error: unable to open log file`
/// （`KOPIA_LOG_DIR` 没指好时）。这些对用户没有意义，删掉，真话留着。
fn clean_stderr(stderr: &str) -> String {
    let cleaned: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains("unable to open log file"))
        .collect();
    let joined = cleaned.join("\n");
    if joined.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_noise_is_dropped_but_the_real_error_survives() {
        let stderr = "2026-09-16 19:20:51 write error: unable to open log file: open /x.log: no such file\n\
                      ERROR failed to connect to repository: invalid password\n";
        let cleaned = clean_stderr(stderr);
        assert!(cleaned.contains("invalid password"), "{cleaned}");
        assert!(!cleaned.contains("unable to open log file"), "{cleaned}");
    }

    #[test]
    fn an_empty_stderr_stays_empty() {
        assert_eq!(clean_stderr("   \n  "), "");
    }

    #[test]
    fn the_default_password_is_the_one_every_machine_shares() {
        // 所有端一致才有"双系统互通"这一说；改它等于把所有老仓库锁在门外。
        assert_eq!(DEFAULT_PASSWORD, "kotori");
    }
}

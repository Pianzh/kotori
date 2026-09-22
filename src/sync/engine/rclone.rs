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
use crate::util::exec::Quiet;

use super::super::archive::{self, Manifest, PackReport};
use super::super::cloud::{self, CloudGame, GameIdentity, PackIdentity};
use super::super::save_targets::SaveTarget;
use super::super::{
    SyncError, copyto_args, deletefile_args, game_remote, list_dirs_args, list_files_args,
    package_remote, parse_packages, rclone_env, remote_root,
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
            .kill_on_drop(true)
            // 同 kopia:后台动作也不许在桌面上闪黑框(见 `util::exec`)。
            .quiet();

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

    /// 云端有哪几款游戏（`games/` 那一层有哪些目录）。
    ///
    /// **先 `mkdir`**：桶里还没有 `games/`（第一次上传之前）不是错误，而 rclone
    /// 列一个不存在的目录会直接失败 —— "云端还没有游戏"该是一句空列表，不是一个
    /// 报错。每个目录再数一遍自己的包，用的是与 [`Self::versions`] 同一条判据。
    pub(super) async fn cloud_games(&self) -> Result<Vec<CloudGame>, SyncError> {
        let games_dir = format!("{}/games", remote_root(&self.settings));
        self.run(&["mkdir".to_string(), games_dir.clone()], COMMAND_TIMEOUT)
            .await?;
        let listed = self
            .run(&list_dirs_args(&games_dir), COMMAND_TIMEOUT)
            .await?;

        let mut games = Vec::new();
        for id in cloud::parse_dirs(&listed) {
            let versions = self.versions(&id).await?.len();
            games.push(CloudGame { id, versions });
        }
        Ok(games)
    }

    pub(super) async fn send(
        &self,
        game_id: &str,
        stamp: &str,
        targets: &[SaveTarget],
        identity: Option<&PackIdentity>,
        work_dir: &Path,
        timeout: Duration,
    ) -> Result<PackReport, SyncError> {
        let zip = work_dir.join(format!("{stamp}{}", super::super::PACKAGE_SUFFIX));
        let report = archive::pack(&zip, targets, chrono::Utc::now(), identity)
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

    // ── 身份卡 ──────────────────────────────────────────────────────────────
    // 身份（"两台机器上哪两条档案是同一款游戏"）在 rclone 那边就是桶里一个 json
    // 文件，与包并排放在 `games/<目录>/` 里。`versions()` 只认包名，所以它不会
    // 被当成一版存档（§5.8）。

    /// 某个目录里的身份卡；目录不存在、或者还没写过卡就是 `None`。
    async fn identity_at(&self, dir: &str) -> Result<Option<GameIdentity>, SyncError> {
        let remote = game_remote(&self.settings, dir);
        let listed = self.run(&list_files_args(&remote), COMMAND_TIMEOUT).await?;
        if !listed
            .lines()
            .any(|name| name.trim() == cloud::IDENTITY_FILE)
        {
            return Ok(None);
        }
        let text = self
            .run(
                &[
                    "cat".to_string(),
                    format!("{remote}/{}", cloud::IDENTITY_FILE),
                ],
                COMMAND_TIMEOUT,
            )
            .await?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| SyncError::Command(format!("云端身份卡读不懂: {e}")))
    }

    /// 这个身份在桶里落在哪个目录 —— 也就是"这一款的包放哪"。
    ///
    /// 顺序是有讲究的：
    ///   1. **云端已经有这个身份的卡** ⇒ 就是那个目录。别的机器先建的目录，我们跟它走：
    ///      版本要能互相看见就得放进**同一个目录**，这正是"游戏名不一样也要绑到同一款"
    ///      这件事在 rclone 那条路上的落点（也是 `GameConfig::cloud_dir` 存在的理由）。
    ///   2. 没有：用游戏 id（人类可读）。
    ///   3. 那个目录被**别的身份**占了：退到 `<id>-<cloud_id 前 6 位>`
    ///      （§5.7：不要用 `-2`，那看着像"同款第二份"）。
    async fn identity_dir(&self, game_id: &str, cloud_id: &str) -> Result<String, SyncError> {
        if let Some(dir) = self.find_identity_dir(cloud_id).await? {
            return Ok(dir);
        }
        for candidate in [
            game_id.to_string(),
            format!("{game_id}-{}", cloud::short_id(cloud_id, 6)),
        ] {
            match self.identity_at(&candidate).await? {
                // 没人占（或者目录还不存在）：就用它。
                None => return Ok(candidate),
                // 已经是我们的卡：还是它。
                Some(identity) if identity.cloud_id == cloud_id => return Ok(candidate),
                // 别人的：换下一个候选。
                Some(_) => continue,
            }
        }
        // 两个候选都被占了（身份唯一，所以这几乎不可能）：用完整身份兜底，不再去抢。
        Ok(format!("{game_id}-{cloud_id}"))
    }

    /// 云端哪个目录里放着这个身份的卡。
    async fn find_identity_dir(&self, cloud_id: &str) -> Result<Option<String>, SyncError> {
        for (dir, identity) in self.read_identities().await? {
            if identity.cloud_id == cloud_id {
                return Ok(Some(dir));
            }
        }
        Ok(None)
    }

    /// 云端与这一款对应的身份卡（读-改-写里的"读"）。
    pub(super) async fn read_identity(
        &self,
        game_id: &str,
        cloud_id: &str,
    ) -> Result<Option<GameIdentity>, SyncError> {
        let dir = self.identity_dir(game_id, cloud_id).await?;
        self.identity_at(&dir).await
    }

    /// 云端**所有**身份卡，带上它所在的目录（"按指纹找同一款"要用）。
    ///
    /// 带目录是因为找到之后要**跟它走同一个目录**（见 [`Self::identity_dir`]）。
    pub(super) async fn read_identities(&self) -> Result<Vec<(String, GameIdentity)>, SyncError> {
        let games_dir = format!("{}/games", remote_root(&self.settings));
        // 桶里还没有 `games/` 时 `lsf` 会失败：先 mkdir（幂等，也顺便把桶建出来）。
        self.run(&["mkdir".to_string(), games_dir.clone()], COMMAND_TIMEOUT)
            .await?;
        let listed = self
            .run(&list_dirs_args(&games_dir), COMMAND_TIMEOUT)
            .await?;

        let mut identities = Vec::new();
        for dir in cloud::parse_dirs(&listed) {
            if let Some(identity) = self.identity_at(&dir).await? {
                identities.push((dir, identity));
            }
        }
        Ok(identities)
    }

    /// 把身份卡放回云端（读-改-写里的"写"；合并由上层做）。
    ///
    /// 返回**这一款在云端该用的键**（rclone 就是那个目录名）：调用方要把它记进配置，
    /// 之后所有版本都往那儿放。
    pub(super) async fn write_identity(
        &self,
        game_id: &str,
        identity: &GameIdentity,
        work_dir: &Path,
    ) -> Result<String, SyncError> {
        let dir = self.identity_dir(game_id, &identity.cloud_id).await?;
        // 身份卡要在**这一步**就写下去，而临时目录可能还没被建出来（上传那条路是
        // 先定身份、再进打包流程，`Staging` 是后建的）。
        std::fs::create_dir_all(work_dir)
            .map_err(|e| SyncError::Command(format!("无法创建 {}: {e}", work_dir.display())))?;
        let local = work_dir.join(cloud::IDENTITY_FILE);
        let text = serde_json::to_string_pretty(identity)
            .map_err(|e| SyncError::Command(format!("身份卡序列化失败: {e}")))?;
        std::fs::write(&local, text)
            .map_err(|e| SyncError::Command(format!("写不了 {}: {e}", local.display())))?;

        let remote = format!(
            "{}/{}",
            game_remote(&self.settings, &dir),
            cloud::IDENTITY_FILE
        );
        self.run(
            &copyto_args(&local.to_string_lossy(), &remote),
            COMMAND_TIMEOUT,
        )
        .await?;
        Ok(dir)
    }
}

/// 一次 rclone 调用的时间上限。
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

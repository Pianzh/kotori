//! 驱动引擎：云同步的编排那一半。
//!
//! [`super::engine`] 知道"这一版怎么上云、怎么取回"，`archive` 知道一个包里有什么；
//! 本模块把两者接起来——解析每个存档位置在**这台机器**上是哪个目录，并把每个位置
//! 的结果如实报出来，供 UI 与 CLI 原样显示。
//!
//! 三条规则贯穿这里（ADR-010 / ADR-012）：
//!   * **绝不毁掉本机数据。** 一版整份上传；启动前的自动取回只取云端更新的那些；
//!     保留窗口只在云上删旧版本。
//!   * **秘密绝不落盘、绝不进命令行**（kopia 的 B2 key 是上游强制的例外，见
//!     [`super::engine::kopia`] 的说明）。
//!   * **失败要说清哪一步失败。** 每个存档位置各有各的结果，绝不用一句"已同步"
//!     盖过某个被跳过的位置。
//!
//! 文件分工：本文件是 [`Runner`] 本身——超时预算与通向引擎的那几个调用；
//! `upload.rs` 管上传，`pull.rs` 管启动前取回，`restore.rs` 管恢复与保留窗口，
//! `staging.rs` 管临时目录与铺文件，`outcome.rs` 是汇报类型，
//! `testing.rs` 是测试用的假引擎。

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::SyncError;
use super::archive::{Manifest, PackReport};
use super::cloud::{CloudGame, PackIdentity};
use super::engine::Backend;
use super::save_targets::SaveTarget;
use super::{validate, validate_secrets};
use crate::config::SyncConfig;
use crate::secrets::Keyring;

pub use self::outcome::{GameOutcome, LocationOutcome};
pub(crate) use self::staging::sweep_stale;

mod outcome;
mod pull;
mod restore;
mod staging;
// 假 rclone 夹具是 shell 脚本,只在 Unix 上能跑(spawn 在 Windows 报 os error
// 193);runner 的测试因此整体 Unix 限定,Windows 覆盖等有 Windows 版假货再补。
#[cfg(all(test, unix))]
mod testing;
#[cfg(all(test, unix))]
mod tests;
mod upload;

/// Ceiling for one engine invocation.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the automatic pull before a launch. Past this the game starts
/// anyway: a slow network must never keep the user out of their game.
pub const PULL_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for「测试连接」(`sync.test`)整件事,不是单条引擎命令。
///
/// 那三步(连桶、必要时建仓库、列一次快照)各自的上限是 [`COMMAND_TIMEOUT`],叠起来
/// 最长十五分钟 —— 而这是一个按钮,用户盯着它等。所以这里给一个明显的总上限,
/// 到点就如实说超时(用户 2026-09-18 报的"点了测试连接没反应")。
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Grace period before uploading after a game exits.
///
/// The session ends when the game's processes are gone, but wine's background
/// services (wineserver) can still be flushing a save to disk. Reading a file
/// mid-write would upload a truncated save, and that truncated copy is what
/// would come back on the next launch, so we wait a moment first.
pub const SETTLE_DELAY: Duration = Duration::from_secs(3);

/// 打包与解包工作区的默认位置（`<数据目录>/sync`）。
///
/// 单独一个函数，是因为 **daemon 启动时要按同一个位置去扫上一次的残骸**
/// （见 [`sweep_stale`]）—— 两处各写各的路径，迟早会分叉。
pub fn default_work_dir() -> PathBuf {
    crate::config::data_dir().join("sync")
}

/// Runs one sync configuration against whichever engine it selects.
pub struct Runner {
    backend: Backend,
    settings: SyncConfig,
    keyring: Keyring,
    /// 打包与解包的落脚点。默认在数据目录下（`~/.local/share/kotori/sync`），
    /// **不放在存档目录旁边**：那儿多出来的临时文件会被下一次打包收进去。
    work_dir: PathBuf,
}

impl Runner {
    /// Build a runner for the configured engine, or say what is missing.
    pub fn new(settings: SyncConfig, keyring: Keyring) -> Result<Self, SyncError> {
        let backend = Backend::new(&settings, keyring.clone())?;
        Ok(Self {
            backend,
            settings,
            keyring,
            work_dir: default_work_dir(),
        })
    }

    /// A runner pinned to one binary. Used by tests, and by users who keep
    /// their tool somewhere unusual (`KOTORI_RCLONE` / `KOTORI_KOPIA` are
    /// handled by `new`).
    #[cfg(all(test, unix))]
    pub fn with_binary(binary: impl Into<PathBuf>, settings: SyncConfig, keyring: Keyring) -> Self {
        let backend = Backend::with_binary(binary.into(), settings.clone(), keyring.clone());
        Self {
            backend,
            settings,
            keyring,
            work_dir: default_work_dir(),
        }
    }

    /// Point the temporary work area somewhere else.
    ///
    /// 只给测试用：一次测试运行绝不该往真实数据目录里写包（生产路径永远走
    /// 数据目录下的 `sync/`）。
    #[cfg(all(test, unix))]
    pub fn with_work_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.work_dir = dir.into();
        self
    }

    /// Structural check plus "are the credentials actually there".
    pub fn ready(&self) -> Result<(), SyncError> {
        validate(&self.settings)?;
        validate_secrets(&self.keyring)
    }

    /// The temporary work area this runner hands to the engine.
    pub(super) fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Verify credentials, bucket and write access in one call.
    ///
    /// 回话里带上**是哪个引擎、哪个二进制**：设置页把它原样显示出来，用户一眼
    /// 就能看出"我连的到底是 rclone 还是 kopia"——两个引擎在桶里各写各的区域，
    /// 认错了不会报错，只会看不见对面的存档。
    pub async fn check(&self) -> Result<String, SyncError> {
        let target = self.backend.check().await?;
        Ok(format!("{} · {target}", self.backend.describe()))
    }

    /// The version packages the cloud holds for a game, oldest first.
    pub async fn packages(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        self.backend.versions(game_id).await
    }

    /// 云端有哪几款游戏（名字有序）。
    ///
    /// 与 [`Self::packages`] 是同一个问题的两级：那一个问"这一款有几版"（前提是
    /// 已经知道 id），这一个问"云端到底有什么"。跨机器可见性靠的就是它。
    pub async fn cloud_games(&self) -> Result<Vec<CloudGame>, SyncError> {
        self.backend.cloud_games().await
    }

    /// The newest package, or `None` when the cloud has never seen this game.
    ///
    /// "Newest" is simply the largest name: the stamp starts with UTC to the
    /// millisecond, so lexicographic order is chronological order and no
    /// pointer file has to be kept in sync. kopia 那条路把同一个名字写进快照的
    /// description，于是这条判据在两个引擎下逐字相同。
    pub async fn latest_package(&self, game_id: &str) -> Result<Option<String>, SyncError> {
        self.backend.latest(game_id).await
    }

    /// Upload this machine's version as one package.
    pub(super) async fn send_version(
        &self,
        game_id: &str,
        stamp: &str,
        targets: &[SaveTarget],
        identity: Option<&PackIdentity>,
        work_dir: &Path,
        timeout: Duration,
    ) -> Result<PackReport, SyncError> {
        self.backend
            .send(game_id, stamp, targets, identity, work_dir, timeout)
            .await
    }

    /// Fetch one version and unpack it into `into`.
    pub(super) async fn fetch_version(
        &self,
        game_id: &str,
        stamp: &str,
        into: &Path,
        timeout: Duration,
    ) -> Result<Manifest, SyncError> {
        self.backend.fetch(game_id, stamp, into, timeout).await
    }

    /// Delete one version package.
    pub(super) async fn remove_version(&self, game_id: &str, stamp: &str) -> Result<(), SyncError> {
        self.backend.remove(game_id, stamp).await
    }
}

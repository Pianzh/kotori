//! 同步引擎：一版存档怎么上云、怎么取回。
//!
//! 上层（`runner` 里的 upload / pull / restore / prune）只说"这一版"，不关心它是
//! 一个 zip 还是一个 kopia 快照。两个引擎由此处收在一个门面后面：
//!
//!   * [`rclone`]：`<游戏id>/<stamp>.zip`，一版一个完整的包。谁有桶的钥匙就能拿
//!     别的工具把它解开——**不加密**。
//!   * [`kopia`]：内容寻址的仓库，去重、增量、自带加密（密码默认 `kotori`）。
//!
//! 两边对外是**同一套版本名**（`version_stamp` 的产物，写进 kopia 快照的
//! description），所以"最新的一版""保留最近 N 版"这些语义在两个引擎下逐字相同，
//! 上面那段代码一行都不用分叉。
//!
//! **一个引擎一个桶区**：rclone 用 `<prefix>/games/<id>/`，kopia 整个仓库落在
//! `<prefix>/kopia`，互不写入。换引擎不会读到对方的布局，也就不会把对方的对象
//! 当成自己的版本（[`super::is_snapshot`] 那道判据只认我们自己的名字）。
//!
//! 工件形态：`send`/`fetch` 只跟**目录**打交道（`work_dir` 与 `into`），具体怎么
//! 摆由引擎自己决定——rclone 在里面放一个 zip，kopia 在里面摆一棵目录树。

use std::path::Path;
use std::time::Duration;

use crate::config::{SyncConfig, SyncEngine};
use crate::secrets::Keyring;

use super::SyncError;
use super::archive::{Manifest, PackReport};
use super::cloud::{CloudGame, GameIdentity, PackIdentity};
use super::index::{CloudIndex, IndexBundle};
use super::save_targets::SaveTarget;

mod diagnostics;
mod kopia;
mod kopia_args;
mod kopia_identity;
mod kopia_index;
mod kopia_index_args;
mod kopia_parse;
mod rclone;
mod rclone_index;
#[cfg(test)]
mod tests;

pub use kopia_args::repo_prefix;
// 换 B2 凭据之后要作废 kopia 的连接记录（BUG-21）：daemon 那边保存凭据时调它，
// 而 kopia 模块本身是私有的。
pub(crate) use kopia::forget_connection;

/// 真正干活的引擎。这个枚举就是"两个引擎"这件事本身。
enum Inner {
    Rclone(Box<rclone::RcloneZip>),
    Kopia(Box<kopia::Kopia>),
}

/// 一个同步后端：设置里的引擎选哪个，这里就是哪个。
pub struct Backend {
    inner: Inner,
}

impl Backend {
    /// 按设置挑一个引擎。
    ///
    /// 二进制不存在时**当场报错**，而不是等到第一次同步：设置页的"测试连接"
    /// 和启动时的环境检查都会走到这里，用户该在那时就看见"没装 kopia"，而不是
    /// 在一次真正要保存进度的时候。
    pub fn new(settings: &SyncConfig, keyring: Keyring) -> Result<Self, SyncError> {
        let inner = match settings.engine {
            SyncEngine::Rclone => {
                Inner::Rclone(Box::new(rclone::RcloneZip::new(settings.clone(), keyring)?))
            }
            SyncEngine::Kopia => {
                Inner::Kopia(Box::new(kopia::Kopia::new(settings.clone(), keyring)?))
            }
        };
        Ok(Self { inner })
    }

    /// 可执行文件在哪、叫什么——只用于日志和"测试连接"的回话。
    pub fn describe(&self) -> String {
        match &self.inner {
            Inner::Rclone(engine) => engine.describe(),
            Inner::Kopia(engine) => engine.describe(),
        }
    }

    /// 凭据、桶与写权限一次验完。
    pub async fn check(&self) -> Result<String, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.check().await,
            Inner::Kopia(engine) => engine.check().await,
        }
    }

    /// 云端有哪些版本，最旧在前。
    pub async fn versions(&self, game_id: &str) -> Result<Vec<String>, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.versions(game_id).await,
            Inner::Kopia(engine) => engine.versions(game_id).await,
        }
    }

    /// 一个游戏在云端的每一版：名字 + 多大 + 什么时候（界面上的"点开看每一版"）。
    pub async fn version_infos(
        &self,
        game_id: &str,
    ) -> Result<Vec<crate::sync::VersionInfo>, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.version_infos(game_id).await,
            Inner::Kopia(engine) => engine.version_infos(game_id).await,
        }
    }

    /// 云端有哪几款游戏。
    ///
    /// 这是"两台机器互相看得见"的那一步（见 [`crate::sync::cloud`]）：从前只有
    /// **已知 id** 才能列版本，第二台机器于是不知道云端有什么。两个引擎的答案在
    /// 这一层是同一句话（谁的 id、有几版），上层因此不用分叉。
    pub async fn cloud_games(&self) -> Result<Vec<CloudGame>, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.cloud_games().await,
            Inner::Kopia(engine) => engine.cloud_games().await,
        }
    }

    /// 最新的一版，云端从没见过这个游戏时是 `None`。
    pub async fn latest(&self, game_id: &str) -> Result<Option<String>, SyncError> {
        Ok(self.versions(game_id).await?.pop())
    }

    /// 把本机这一版送上去，返回"装了什么、谁不在"。
    ///
    /// 工件摆在 `work_dir` 里（调用方给一个刚建好的临时目录），由引擎决定形态。
    pub async fn send(
        &self,
        game_id: &str,
        stamp: &str,
        targets: &[SaveTarget],
        identity: Option<&PackIdentity>,
        work_dir: &Path,
        timeout: Duration,
    ) -> Result<PackReport, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => {
                engine
                    .send(game_id, stamp, targets, identity, work_dir, timeout)
                    .await
            }
            Inner::Kopia(engine) => {
                engine
                    .send(game_id, stamp, targets, identity, work_dir, timeout)
                    .await
            }
        }
    }

    /// 把某一版取回来，解到 `into`，返回它的清单。
    pub async fn fetch(
        &self,
        game_id: &str,
        stamp: &str,
        into: &Path,
        timeout: Duration,
    ) -> Result<Manifest, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.fetch(game_id, stamp, into, timeout).await,
            Inner::Kopia(engine) => engine.fetch(game_id, stamp, into, timeout).await,
        }
    }

    /// 云端与这一款对应的**身份卡**（读-改-写里的"读"）。
    ///
    /// 两个引擎在这一层仍然是同一句话：rclone 读桶里那份 json，kopia 把最新那条身份
    /// 快照恢复出来读。`work_dir` 是读它需要的落脚点（kopia 要解到磁盘上）。
    pub async fn read_identity(
        &self,
        game_id: &str,
        cloud_id: &str,
        work_dir: &Path,
    ) -> Result<Option<GameIdentity>, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.read_identity(game_id, cloud_id).await,
            Inner::Kopia(engine) => engine.read_identity(game_id, cloud_id, work_dir).await,
        }
    }

    /// 云端**所有**身份卡，带上各自在云端的键（"按 exe 指纹找同一款"要用）。
    ///
    /// 键对 rclone 是那个目录名、对 kopia 就是身份本身 —— 找到之后要**跟它走同一个
    /// 键**，版本才会落在同一处、两台机器才互相看得见。
    pub async fn read_identities(
        &self,
        work_dir: &Path,
    ) -> Result<Vec<(String, GameIdentity)>, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.read_identities().await,
            Inner::Kopia(engine) => engine.read_identities(work_dir).await,
        }
    }

    /// 写回一张身份卡，返回**这一款在云端该用的键**（读-改-写里的"写"；合并由上层做）。
    pub async fn write_identity(
        &self,
        game_id: &str,
        identity: &GameIdentity,
        work_dir: &Path,
    ) -> Result<String, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.write_identity(game_id, identity, work_dir).await,
            Inner::Kopia(engine) => engine.write_identity(game_id, identity, work_dir).await,
        }
    }

    /// 读云端索引：合并快照 + **还没并进去**的那些增量（一个桶一份，见 `crate::sync::index`）。
    pub async fn read_index(&self, work_dir: &Path) -> Result<IndexBundle, SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.read_index().await,
            Inner::Kopia(engine) => engine.read_index(work_dir).await,
        }
    }

    /// 写一条索引增量（**必须先写它**：对象名唯一，谁也覆盖不了谁）。
    pub async fn write_index_delta(
        &self,
        name: &str,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.write_index_delta(name, index, work_dir).await,
            Inner::Kopia(engine) => engine.write_index_delta(name, index, work_dir).await,
        }
    }

    /// 重写索引的合并快照（**后写它**）。
    pub async fn write_index_main(
        &self,
        index: &CloudIndex,
        work_dir: &Path,
    ) -> Result<(), SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.write_index_main(index, work_dir).await,
            Inner::Kopia(engine) => engine.write_index_main(index, work_dir).await,
        }
    }

    /// 删掉某一版（保留窗口用）。
    pub async fn remove(&self, game_id: &str, stamp: &str) -> Result<(), SyncError> {
        match &self.inner {
            Inner::Rclone(engine) => engine.remove(game_id, stamp).await,
            Inner::Kopia(engine) => engine.remove(game_id, stamp).await,
        }
    }
}

#[cfg(test)]
impl Backend {
    /// 钉在一个指定的可执行文件上，按设置里的引擎挑路。
    ///
    /// 测试不许依赖 PATH（CI 的规矩）：假引擎由此注入。
    pub fn with_binary(binary: std::path::PathBuf, settings: SyncConfig, keyring: Keyring) -> Self {
        let inner = match settings.engine {
            SyncEngine::Rclone => Inner::Rclone(Box::new(rclone::RcloneZip::with_binary(
                binary, settings, keyring,
            ))),
            SyncEngine::Kopia => Inner::Kopia(Box::new(kopia::Kopia::with_binary(
                binary, settings, keyring,
            ))),
        };
        Self { inner }
    }
}

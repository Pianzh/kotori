//! kotori 用过的 wine prefix —— 注销 / 关机时的最后一道兜底。
//!
//! 一局游戏收尾时,每个会话都会对**自己那个** prefix 跑一次 `wineserver -k`
//! (见 [`crate::wine::close_prefix`])。那一步只覆盖"还活着、还归 kotori 管"的会话,
//! 而 2026-09-13 真机上卡关机的两个 `winedevice.exe` 说明了缺口在哪:
//!
//! - 它**无视 SIGTERM**,又待在自己的进程组里(`/proc/<pid>/stat` 里的 pgid 就是它自己),
//!   父进程早就没了(`PPid: 1`)⇒ **进程组击杀与进程树击杀都碰不到它**,
//!   唯一收得掉它的是 `wineserver -k`(2026-09-13 用真 wine 验证过:一发 `-k`,
//!   `winedevice.exe` ×2 与 `services.exe`/`plugplay.exe`/`explorer.exe` 一起消失);
//! - 一旦它是**某个已经死掉的 daemon** 留下的(SIGKILL、或者被顶掉),下一个 daemon
//!   根本不知道它存在 ⇒ 没人关它 ⇒ 注销 / 关机时 systemd 只能在这个 scope 上等满
//!   90 秒 `TimeoutStopSec`,用户看到的就是"关机卡住"。
//!
//! 所以这里把用过的 prefix **记在数据目录里**。它不是配置,是运行时状态:
//! 用户不该在 `config.toml` 里看到它,删掉它的代价只是丢掉这道兜底。
//!
//! ⚠ **只在这一条路上清扫**(收到 SIGTERM/SIGINT、把在跑的会话收完之后):
//! 名单里可能有用户自己也在用的 prefix,而"关掉自己用过的 prefix"在注销 / 关机那一刻
//! 正是想要的,在 `daemon.shutdown`(设置页的「停止服务」)那一刻就不是了 ——
//! 那两个出口分得很清(ADR-002 / ADR-017),这里跟它们保持一致。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 名单文件名,放在 [`crate::config::data_dir`] 里。
const FILE_NAME: &str = "wine-prefixes";

/// 整轮清扫的时间上限。
///
/// 单个 prefix 自己已经有 2 秒上限(`wine::WINESERVER_KILL_TIMEOUT`),但名单会长,
/// 不能让"收尾"变成新的卡住:关机时我们最多花这么久,剩下的交给 systemd 的超时。
const SWEEP_BUDGET: Duration = Duration::from_secs(20);

/// 记下 kotori 在哪个 prefix 下起过游戏。
///
/// 失败只记日志:记不住的代价是"下次关机可能留个残留",不该影响这一局开始。
pub fn record(prefix: &Path) {
    let dir = crate::config::data_dir();
    if let Err(err) = record_at(&dir, prefix) {
        tracing::warn!(
            "记不住 wine prefix {}（{err}）—— 万一这局之后没机会收尾,关机时就没得兜底了",
            prefix.display()
        );
    }
}

/// [`record`] 的显式目录版(测试用它,不会碰到真机的数据目录)。
pub fn record_at(dir: &Path, prefix: &Path) -> std::io::Result<()> {
    if recorded_at(dir).iter().any(|known| known == prefix) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE_NAME))?;
    writeln!(file, "{}", prefix.display())
}

/// 名单上的 prefix,按记下的顺序去重。
///
/// 文件不存在、读不了都当"没有" —— 这只是兜底,不值得为它报错。
/// 目录已经不存在的条目**不在这里**丢掉:那是调用方的事(见 [`close_all_with`]),
/// 因为"文件里写着什么"和"该不该为它起个子进程"是两件事。
pub fn recorded_at(dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE_NAME)) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let path = PathBuf::from(line);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// 把名单上**还存在**的 prefix 逐个关掉,返回试了几个。
pub async fn close_all() -> usize {
    close_all_with(
        &crate::wine::wineserver_binary(),
        &crate::config::data_dir(),
    )
    .await
}

/// [`close_all`] 的显式版本(测试注入假 `wineserver` 与临时目录)。
pub async fn close_all_with(binary: &Path, dir: &Path) -> usize {
    let deadline = Instant::now() + SWEEP_BUDGET;
    let mut tried = 0;
    for prefix in recorded_at(dir) {
        // 目录都没了 ⇒ 那儿不可能还有 server 在跑,省一次子进程。
        if !prefix.is_dir() {
            continue;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                "wine 残留没清完（{:?} 用尽），剩下的交给 systemd 的超时",
                SWEEP_BUDGET
            );
            break;
        }
        crate::wine::close_prefix_with(binary, &prefix).await;
        tried += 1;
    }
    if tried > 0 {
        tracing::info!(
            "顺手关掉 {tried} 个用过的 wine prefix（没人认领的 winedevice.exe 只有这一步收得掉）"
        );
    }
    tried
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个会自己取名的临时目录,并行测试之间不会撞车。
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kotori-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假的 `wineserver`:每被调用一次就往 `calls.txt` 追一行。
    fn recording_wineserver(dir: &Path) -> PathBuf {
        let script = dir.join("wineserver");
        crate::secrets::testing::write_executable(
            &script,
            &format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--kotori-warmup\" ]; then exit 0; fi\n\
                 echo \"args=$* prefix=$WINEPREFIX\" >> {}\n",
                dir.join("calls.txt").display()
            ),
        );
        script
    }

    #[test]
    fn a_prefix_is_only_written_down_once() {
        let dir = scratch("prefixes-dedup");
        let prefix = dir.join("prefix");

        record_at(&dir, &prefix).unwrap();
        record_at(&dir, &prefix).unwrap();
        record_at(&dir, &dir.join("other")).unwrap();

        assert_eq!(recorded_at(&dir), vec![prefix, dir.join("other")]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_just_means_nothing_was_recorded() {
        let dir = scratch("prefixes-missing");
        assert!(recorded_at(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_sweep_closes_what_still_exists_and_skips_what_is_gone() {
        let dir = scratch("prefixes-sweep");
        let script = recording_wineserver(&dir);
        let live = dir.join("live-prefix");
        std::fs::create_dir_all(&live).unwrap();
        record_at(&dir, &live).unwrap();
        record_at(&dir, &dir.join("deleted-prefix")).unwrap();

        let tried = close_all_with(&script, &dir).await;

        assert_eq!(tried, 1, "只有还存在的那个该被关");
        let calls = std::fs::read_to_string(dir.join("calls.txt")).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
        assert!(calls.contains("args=-k"), "{calls}");
        assert!(
            calls.contains(&live.display().to_string()),
            "关的必须是记下来的那个 prefix: {calls}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn an_empty_list_runs_no_helper_at_all() {
        let dir = scratch("prefixes-empty");
        let script = recording_wineserver(&dir);

        assert_eq!(close_all_with(&script, &dir).await, 0);
        assert!(
            !dir.join("calls.txt").exists(),
            "名单是空的时候不该起子进程"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

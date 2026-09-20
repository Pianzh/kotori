//! 自动追踪:**不是 kotori 启动的游戏,也该有一局记录。**
//!
//! 用户 2026-09-20 的原话:「开启仅观测应该代表即使游戏不是 kotori 启动的,kotori
//! 也会追踪进程并记录存档」。从前这件事得由用户**点一次「启动」**才发生 —— 那一按
//! 什么也不启动,只是让 daemon 开始盯进程名。于是"我双击图标玩的那些局"在库里
//! 什么都不留,退出后的上传自然也不触发(`Ended` 是上传的唯一触发器)。
//!
//! 这里的循环替掉那一按:每隔 [`WATCH_POLL`] 看一眼配置里开着 `auto_watch` 的游戏,
//! 谁的名字在进程表里、而且**还没有会话**,就替它开一个观测会话(`ScaleSession.watch_only`)
//! —— 会话本身照旧由 `scale::direct` 跟到进程消失,再发 `Ended`。
//!
//! 两条防重复的规矩,都不是锦上添花:
//!
//! * **按 game_id 去重**:用户从 kotori 点「启动」开的那一局,`rpc_game_launch` 早就
//!   登记过会话了,这里不能再开一个(否则退出时会传两次,界面也只认得下一条)。
//! * **连续两次都看到才算数**:`spawn` 与"登记进会话表"之间有一小段窗口
//!   (`direct` 里那 300ms 的立即退出检测),正好落在窗口里的话也会重复。多等一个
//!   轮询周期把窗口躲开,顺带滤掉一闪而过的短命进程。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::*;

/// 多久看一次进程表。与 `process::POLL_INTERVAL` 同量级:它决定的是"游戏开起来
/// 多久之后库里才显示在跑",2 秒足够,而每次只是读一遍进程表。
pub(super) const WATCH_POLL: Duration = Duration::from_secs(2);

/// 一个候选:配置里开着自动追踪、而且能推出进程名的一款游戏。
struct Watched {
    id: String,
    name: String,
    game_dir: PathBuf,
    profile: crate::config::ScaleProfile,
}

impl Daemon {
    /// 起这个循环。`run()` 里跟在 `spawn_sync_events` 后面调一次。
    pub(super) fn spawn_process_watch(&self) {
        let this = self.clone_shares();
        tokio::spawn(async move {
            // 名字第一次被看到的时刻。用来实现"连续两次"那条规矩。
            let mut first_seen: HashMap<String, Instant> = HashMap::new();
            // 已经报过错的游戏:进程还在就一直别重复刷屏。
            let mut complained: HashSet<String> = HashSet::new();
            loop {
                tokio::time::sleep(WATCH_POLL).await;
                let candidates = this.auto_watch_candidates().await;
                // 没在候选里 = 进程早就没了(或用户关了开关),两本账一起收干净 ——
                // 解禁也在这里发生:进程走光之后,下一局照样自动跟。
                let ids: HashSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
                first_seen.retain(|id, _| ids.contains(id.as_str()));
                complained.retain(|id| ids.contains(id.as_str()));
                this.ignored_watch
                    .write()
                    .await
                    .retain(|id, _| ids.contains(id.as_str()));

                let live = this.engine.list_sessions().await;
                // 这一轮已经"认领"过的进程名。⚠ 光看 `live` 不够:同一轮里先开的
                // 那个会话还没进 `live`(它是在循环开始前取的),于是同一个进程会被
                // 第二款游戏再认一次 —— 用户库里就有两款 exe 都叫 `Game.exe`
                // (2026-09-20 实测:一轮里开出两个会话)。
                let mut claimed: Vec<String> = live
                    .iter()
                    .filter_map(|session| session.process_name.clone())
                    .collect();
                for game in candidates {
                    // 用户亲手停掉的那一款:同一个进程实例还在就闭嘴
                    // (见 `Daemon::ignored_watch`)。
                    if this.this_run_was_stopped(&game).await {
                        continue;
                    }
                    if live
                        .iter()
                        .any(|session| session.game_id.as_deref() == Some(game.id.as_str()))
                    {
                        continue;
                    }
                    // 同一个进程名只能归一款:进程名认不出是哪一款时(比如两款游戏
                    // 都叫 `Game.exe`)按 id 排序取第一个,并在日志里说清这是歧义 ——
                    // 用户给它们各填一个「跟随的进程」就能消除。
                    if let Some(other) = claimed
                        .iter()
                        .find(|name| crate::process::same_name(name, &game.name))
                    {
                        tracing::warn!(
                            "进程 {} 同时匹配多款开着自动追踪的游戏,这一轮只跟 {};给它们填「跟随的进程」可以消除歧义",
                            game.name,
                            other
                        );
                        continue;
                    }
                    let seen = *first_seen
                        .entry(game.id.clone())
                        .or_insert_with(Instant::now);
                    if seen.elapsed() < WATCH_POLL {
                        continue;
                    }
                    if complained.contains(&game.id) {
                        continue;
                    }
                    match this.start_auto_watch(&game).await {
                        Ok(session_id) => {
                            tracing::info!(
                                "自动追踪:{}（{}）已经在跑,开一个观测会话 {session_id}",
                                game.id,
                                game.name
                            );
                            claimed.push(game.name.clone());
                            first_seen.remove(&game.id);
                        }
                        Err(error) => {
                            tracing::warn!("自动追踪 {} 失败:{error}", game.id);
                            complained.insert(game.id.clone());
                        }
                    }
                }
            }
        });
    }

    /// 这一刻值得自动追踪的游戏 —— **只挑进程真的在跑的**。
    ///
    /// 进程表只读一遍(`Snapshot`):42 款游戏逐个 `is_running` 就是每 2 秒 42 遍
    /// 遍历,而这件事每 2 秒发生一次、永远不停。
    async fn auto_watch_candidates(&self) -> Vec<Watched> {
        let wanted: Vec<Watched> = {
            let config = self.config.read().await;
            config
                .games
                .iter()
                .filter(|(_, game)| game.auto_watch)
                .filter_map(|(id, game)| {
                    let name = game.watch_name()?;
                    Some(Watched {
                        id: id.clone(),
                        name,
                        game_dir: game.effective_game_dir(),
                        profile: game.scale_profile.clone(),
                    })
                })
                .collect()
        };

        let running = crate::process::Snapshot::take();
        let mut found: Vec<Watched> = wanted
            .into_iter()
            .filter(|game| running.matches(&game.name))
            .collect();
        // 名字有歧义时(同一轮里好几款都匹配)"取第一个"得有个确定的说法 ——
        // 配置里是 HashMap,遍历顺序每次都可能不同。
        found.sort_by(|left, right| left.id.cmp(&right.id));
        found
    }

    /// 用户是不是刚亲手停掉了**这一次运行**?
    ///
    /// 按 pid 认:停止那一刻这个进程名有哪几个 pid,现在就还是哪几个 —— 游戏退出后
    /// 用户重开得到的是新 pid,于是照旧被自动追踪。
    async fn this_run_was_stopped(&self, game: &Watched) -> bool {
        let ignored = self.ignored_watch.read().await;
        let Some(stopped) = ignored.get(&game.id) else {
            return false;
        };
        let live = crate::process::find_pids(&game.name);
        stopped.iter().any(|pid| live.contains(pid))
    }

    /// 替一款已经在跑的游戏开观测会话:什么都不启动,只跟着它。
    async fn start_auto_watch(&self, game: &Watched) -> Result<String, String> {
        let spec = LaunchSpec {
            game_id: &game.id,
            exe: "",
            args: &[],
            game_dir: &game.game_dir,
            // 不是我们启动的,所以没有"我们的 prefix"要收尾(见 `direct::start_watch_session`)。
            wine_prefix: None,
            profile: &game.profile,
            process_name: Some(&game.name),
            watch_only: true,
            direct_launch: false,
        };
        let session = self
            .engine
            .start_session(&spec)
            .await
            .map_err(|e| e.to_string())?;
        Ok(session.session_id)
    }
}

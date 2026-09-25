//! The `game.*` requests: the library, and launching or stopping one game.
//!
//! Anything here that writes goes through [`Daemon::mutate_config`], so the
//! daemon stays the only writer of the config file.

use serde_json::{Value, json};

use super::*;

impl Daemon {
    /// Return every configured game as its full `GameConfig` plus `id`.
    ///
    /// Serializing the whole struct (instead of hand-picking fields) is what
    /// keeps clients from silently dropping `force_fullscreen`,
    /// `framerate_limit` and sharpness.
    pub(super) async fn rpc_game_list(&self) -> Result<Value, String> {
        let config = self.config.read().await;
        let mut games: Vec<Value> = config
            .games
            .iter()
            .map(|(id, game)| {
                let mut value = game.location_view();
                if let Value::Object(map) = &mut value {
                    map.insert("id".to_string(), Value::String(id.clone()));
                }
                value
            })
            .collect();
        // HashMap iteration order is random; keep the library stable.
        //
        // ⚠ 排序键必须是**唯一的**,所以用 `(name, id)` 而不是光看 `name`。用户
        // 2026-09-18 点的就是这个:「按 name 排序我认为应该是错的,你没有考虑 name
        // 相同的极端状态」。同名两条时,光按 name 排出来的相对顺序由 `HashMap` 的
        // 遍历顺序决定 —— 它随插入历史与每次启动的随机种子变,于是"列表顺序"这件事
        // 就没有一个说法。id 生成出来就唯一且不再变,拿它当第二关键字既保住"按名字
        // 字母序"这个直觉,又给出一个真正的全序。
        games.sort_by(|a, b| {
            let key = |v: &Value| {
                (
                    v.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    v.get("id")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            };
            key(a).cmp(&key(b))
        });

        Ok(json!({ "games": games }))
    }

    pub(super) async fn rpc_game_remove(&self, id: &str) -> Result<Value, String> {
        self.mutate_config(|config| {
            if !crate::game::remove_game(config, id) {
                return Err(format!("配置中找不到游戏: {id}"));
            }
            tracing::info!("game.remove: {id}");
            Ok(json!({ "success": true }))
        })
        .await
    }

    /// 扫一个目录,把它找到的游戏加进库里 —— `kotori add` 走的就是这条。
    ///
    /// 从前 `kotori add` 自己 `load()`/`save()` 配置,CLI 因此成了**第二个写者**
    /// (BUG-16):它与这里(UI 的自动保存)在同一毫秒落地时,谁后写谁赢,先写的那
    /// 一笔就没了。收进 daemon 之后只有一个写者,而且加完**内存与磁盘同一步更新**
    /// —— GUI 不用等下一次刷新就能看到新游戏。
    ///
    /// 返回值的形状与 CLI 打印的一一对应:`added` 是真正新加进去的(已存在的 exe
    /// 会被跳过,见 `game::add_games`),`warnings` 是"这个 exe 已经被别的档案用
    /// 着"的提醒。两件事都在 daemon 侧算,因为只有它手里是最新的那份配置。
    pub(super) async fn rpc_game_add(&self, directory: &Path) -> Result<Value, String> {
        if !directory.is_dir() {
            return Err(format!("目录不存在: {}", directory.display()));
        }
        let found = crate::game::scan(directory).map_err(|e| e.to_string())?;
        self.mutate_config(move |config| {
            let added = crate::game::add_games(config, found);
            let games: Vec<Value> = added
                .iter()
                .map(|(id, game)| {
                    json!({
                        "id": id,
                        "name": game.name,
                        "exe_path": game.exe_path.display().to_string(),
                    })
                })
                .collect();
            let warnings: Vec<Value> = added
                .iter()
                .filter_map(|(id, game)| {
                    crate::game::duplicate_exe_warning(config, &game.exe_path, Some(id))
                        .map(|message| json!({ "id": id, "message": message }))
                })
                .collect();
            Ok(json!({ "added": games, "warnings": warnings }))
        })
        .await
    }

    /// 探测一个路径落在哪块盘上:UI 在用户挑完目录/exe 之后调它,把「挂载盘号 +
    /// 磁盘内相对目录」两栏自动填好(用户 2026-09-25 定的交互)。
    ///
    /// `{"mount": null}` 表示这个路径**不在**任何能认出 UUID 的盘上 —— 那就照老规矩
    /// 存绝对路径。探测只读,不写配置。
    pub(super) async fn rpc_mount_infer(&self, path: &Path) -> Result<Value, String> {
        let inferred = crate::mount::MountTable::read().infer(path);
        Ok(json!({ "mount": inferred }))
    }

    pub(super) async fn rpc_game_launch(&self, id: &str, selfcheck: bool) -> Result<Value, String> {
        let (game, wine_prefix, prefix_source) = {
            let config = self.config.read().await;
            let Some(game) = config.games.get(id).cloned() else {
                return Err(format!("配置中找不到游戏: {id}"));
            };
            let (prefix, source) = crate::wine::resolve_prefix(&game, &config);
            (game, prefix, source)
        };

        let mounts = crate::mount::MountTable::read();
        let game_dir = game.resolved_game_dir_with(&mounts)?;
        let exe_path = game.resolved_exe_with(&mounts)?;
        if !game_dir.is_dir() || !exe_path.is_file() {
            return Err("游戏目录或可执行文件不存在，请检查磁盘和游戏位置".into());
        }

        // 云同步自检（用户 2026-09-22："云同步（打开游戏）前必须自检"）。整条路上
        // **只有一种情况会打断用户**：未定、指纹又认不出来 —— 那时**先不起游戏**，
        // 请界面问一次（`sync.resolve`），问完再调一次本方法。其余结论（跳过 /
        // 已确认 / 静默认领 / 直接新建）都在这里落盘，然后照旧往下走。
        // ⚠ "问一次"要客户端**先声明它答得上来**（`selfcheck: true`）：界面还没接那一
        // 问之前，这条路上返回"要决定"就等于让用户点不动「启动」—— 一个真回归。其余
        // 结论（静默认领 / 已确认 / 跳过）不需要界面配合，一律照做。
        let decision = self.sync_selfcheck(id).await;
        if matches!(decision, crate::sync::selfcheck::Decision::Ask { .. }) {
            if selfcheck {
                tracing::info!("{id}: 启动前要问一次配对（指纹认不出云端那一条）");
                // 把"疑似找到的那一条"一起带回界面：有就显示它（名字与摘要由界面用**同一个
                // 函数**生成），没有就是"完全没找到"。用户 2026-09-24 要弹窗说清云端那款叫
                // 什么，否则他没法定夺。
                let cloud = match &decision {
                    crate::sync::selfcheck::Decision::Ask { found: Some(found) } => json!({
                        "cloud_id": &found.cloud_id,
                        "cloud_key": &found.cloud_key,
                        "name": &found.name,
                        "versions": found.versions,
                        "latest": &found.latest,
                        "size": found.size,
                    }),
                    _ => serde_json::Value::Null,
                };
                return Ok(json!({ "needs_sync_decision": true, "cloud": cloud }));
            }
            tracing::debug!("{id}: 认不出云端那一条，但客户端答不了这一问 —— 照旧启动");
        }
        if let Err(error) = self.apply_decision(id, &decision).await {
            // 自检的结论写不下**绝不能**拦住启动：用户要的是玩游戏。
            tracing::warn!("{id}: 自检结论没能落盘: {error}");
        }

        // Fetch the newest saves *before* the game can read them. Best effort on
        // a deadline: a broken backup must never keep the user out of their game
        // (see `sync_pull_before_launch`). `None` means sync had nothing to do.
        let pulled = self.sync_pull_before_launch(id).await;

        // ⚠ 这里**没有**"仅观测的游戏不许启动"那条分支了。开着自动追踪只是说
        // "别人启动的那一局也要跟",它跟"谁把它启动起来"无关;从前那句
        // 「是「仅观测」模式」是拿跟踪当成启动方式的替代品(用户 2026-09-20 纠正)。
        // 自动追踪由 `daemon::watch` 的后台循环负责,与这条路径互不干涉:
        // 它看到本会话已经存在就不会再开一个。

        tracing::info!(
            "launching {} (cwd={} prefix={} ← {})",
            game.name,
            game_dir.display(),
            wine_prefix.display(),
            prefix_source.label()
        );

        let exe = exe_path.to_string_lossy().to_string();
        let spec = LaunchSpec {
            game_id: id,
            exe: &exe,
            args: &game.launch_args,
            game_dir: &game_dir,
            wine_prefix: Some(&wine_prefix),
            profile: &game.scale_profile,
            process_name: game.process_name.as_deref(),
            watch_only: false,
            direct_launch: game.direct_launch,
        };

        let session = self
            .engine
            .start_session(&spec)
            .await
            .map_err(|e| e.to_string())?;

        Ok(json!({
            "session_id": session.session_id,
            "gamescope_pid": session.gamescope_pid,
            "game_dir": game_dir,
            "wine_prefix": wine_prefix,
            "prefix_source": prefix_source.label(),
            "sync_pull": pulled,
        }))
    }

    pub(super) async fn rpc_game_wait(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .wait_session(&session)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "exited": true }))
    }

    pub(super) async fn rpc_game_stop(&self, session_id: &str) -> Result<Value, String> {
        let session = self.lookup_session(session_id).await?;
        self.engine
            .stop_session(&session)
            .await
            .map_err(|e| e.to_string())?;
        // 观测会话是后台循环自己认出来的,而那个循环还会再看到同一个进程 ——
        // 不记一笔的话,用户点一次「停止」两秒后就被那个循环撤销了。这一笔在进程
        // 走光时自动清掉(见 `daemon::watch`),所以只是"这一局别再跟了"。
        if session.watch_only
            && let Some(game_id) = &session.game_id
        {
            // 记的是"停的那一刻这个名字有哪几个 pid":游戏退出后用户重开拿到的是新
            // pid,于是下一局照旧被自动追踪(见 `Daemon::ignored_watch`)。
            let pids = session
                .process_name
                .as_deref()
                .map(crate::process::find_pids)
                .unwrap_or_default();
            self.ignored_watch
                .write()
                .await
                .insert(game_id.clone(), pids);
        }
        Ok(json!({ "success": true }))
    }
}

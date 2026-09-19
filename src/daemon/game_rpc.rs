//! The `game.*` requests: the library, and launching or stopping one game.
//!
//! Anything here that writes goes through [`Daemon::mutate_config`], so the
//! daemon stays the only writer of the config file.

use serde_json::{Value, json};

use super::protocol::{GamePatch, NewGame};
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
                let mut value = serde_json::to_value(game).unwrap_or_else(|e| {
                    tracing::warn!("failed to serialize game {id}: {e}");
                    json!({})
                });
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

    /// Create a library entry from explicit user input (the manual add path —
    /// no scanning heuristics involved).
    pub(super) async fn rpc_game_create(&self, new_game: NewGame) -> Result<Value, String> {
        let name = new_game.name.trim().to_string();
        if name.is_empty() {
            return Err("名称不能为空".to_string());
        }
        if !new_game.exe_path.is_file() {
            return Err(format!("可执行文件不存在: {}", new_game.exe_path.display()));
        }

        let game_dir = match new_game.game_dir {
            Some(dir) if !dir.as_os_str().is_empty() => {
                if !dir.is_dir() {
                    return Err(format!("游戏目录不存在: {}", dir.display()));
                }
                dir
            }
            // Default to where the exe lives.
            _ => new_game
                .exe_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        };

        self.mutate_config(|config| {
            // 同名冲突在 `generate_unique_game_id` 里已经用后缀解决了 —— 同一款
            // 游戏建两条档案是合法需求,报错只会把它挡在门外。id 在写锁内生成,
            // 两个并发 create 不会抢到同一个。
            let id = crate::game::generate_unique_game_id(config, &name);
            if id.is_empty() {
                return Err("这个名称无法生成合法的游戏 ID，请换一个".to_string());
            }
            let warning = crate::game::duplicate_exe_warning(config, &new_game.exe_path, None);
            config.games.insert(
                id.clone(),
                crate::config::GameConfig {
                    name: name.clone(),
                    game_dir: game_dir.clone(),
                    exe_path: new_game.exe_path.clone(),
                    launch_args: Vec::new(),
                    save_paths: Vec::new(),
                    wine_prefix: None,
                    watch_only: false,
                    process_name: None,
                    scale_profile: crate::config::ScaleProfile::default_for(),
                    created_at: chrono::Utc::now(),
                },
            );
            if let Some(text) = &warning {
                tracing::info!("game.create: {id} (duplicate exe warning: {text})");
            } else {
                tracing::info!("game.create: {id}");
            }
            let mut result = json!({ "id": id, "name": name });
            if let Some(text) = warning {
                result["warning"] = json!(text);
            }
            Ok(result)
        })
        .await
    }

    /// Patch the mutable fields of a game. This is the only way a client
    /// persists game settings (the daemon is the single writer of the config).
    pub(super) async fn rpc_game_update(
        &self,
        id: &str,
        patch: GamePatch,
    ) -> Result<Value, String> {
        self.mutate_config(|config| {
            // Save paths are validated by resolving them, which needs an
            // immutable view of the game *and* the config; take that before
            // mutating anything.
            if let Some(save_paths) = &patch.save_paths {
                let snapshot = config
                    .games
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;
                let mut candidate = snapshot;
                if let Some(dir) = &patch.game_dir {
                    candidate.game_dir = dir.clone();
                }
                let (root, _) = crate::wine::SaveRoot::for_platform(&candidate, config);
                let game_dir = candidate.effective_game_dir();
                for save in save_paths {
                    crate::wine::resolve_save_path(&root, &game_dir, save)?;
                }
            }

            let game = config
                .games
                .get_mut(id)
                .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;

            if let Some(name) = &patch.name {
                if name.trim().is_empty() {
                    return Err("名称不能为空".to_string());
                }
                game.name = name.clone();
            }

            if let Some(dir) = &patch.game_dir {
                if !dir.is_dir() {
                    return Err(format!("游戏目录不存在: {}", dir.display()));
                }
                game.game_dir = dir.clone();
            }

            if let Some(exe) = &patch.exe_path {
                if !exe.is_file() {
                    return Err(format!("可执行文件不存在: {}", exe.display()));
                }
                game.exe_path = exe.clone();
            }

            if let Some(args) = &patch.launch_args {
                game.launch_args = args.clone();
            }

            if let Some(save_paths) = &patch.save_paths {
                game.save_paths = save_paths.clone();
            }

            // `null` clears an optional field; an absent key leaves it alone.
            if let Some(prefix) = &patch.wine_prefix {
                game.wine_prefix = prefix.clone();
            }
            if let Some(process_name) = &patch.process_name {
                game.process_name = process_name.clone().filter(|name| !name.trim().is_empty());
            }

            if let Some(watch_only) = patch.watch_only {
                game.watch_only = watch_only;
            }

            if let Some(profile) = &patch.profile {
                let mut parsed = profile.clone();
                parsed.normalize();
                parsed.validate()?;
                game.scale_profile = parsed;
            }

            tracing::info!("game.update: {id}");
            Ok(json!({ "success": true }))
        })
        .await
    }

    pub(super) async fn rpc_game_launch(&self, id: &str) -> Result<Value, String> {
        let (game, wine_prefix, prefix_source) = {
            let config = self.config.read().await;
            let Some(game) = config.games.get(id).cloned() else {
                return Err(format!("Game not found: {id}"));
            };
            let (prefix, source) = crate::wine::resolve_prefix(&game, &config);
            (game, prefix, source)
        };

        let game_dir = game.effective_game_dir();

        // Fetch the newest saves *before* the game can read them. Best effort on
        // a deadline: a broken backup must never keep the user out of their game
        // (see `sync_pull_before_launch`). `None` means sync had nothing to do.
        let pulled = self.sync_pull_before_launch(id).await;

        // Watch-only: kotori never launches these, it just follows the process
        // so clients (and save sync) know when the game runs.
        if !game.is_launchable() {
            let Some(name) = game.process_name.as_deref() else {
                return Err(format!(
                    "「{}」是「仅观测」模式，但没有填写要观测的进程名；请在详情页里补上",
                    game.name
                ));
            };
            let spec = LaunchSpec {
                game_id: id,
                exe: "",
                args: &[],
                game_dir: &game_dir,
                wine_prefix: None,
                profile: &game.scale_profile,
                process_name: Some(name),
                watch_only: true,
            };
            let session = self
                .engine
                .start_session(&spec)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(json!({
                "session_id": session.session_id,
                "watch_only": true,
                "process_name": name,
                "game_dir": game_dir,
                "sync_pull": pulled,
            }));
        }

        tracing::info!(
            "launching {} (cwd={} prefix={} ← {})",
            game.name,
            game_dir.display(),
            wine_prefix.display(),
            prefix_source.label()
        );

        let exe = game.exe_path.to_string_lossy().to_string();
        let spec = LaunchSpec {
            game_id: id,
            exe: &exe,
            args: &game.launch_args,
            game_dir: &game_dir,
            wine_prefix: Some(&wine_prefix),
            profile: &game.scale_profile,
            process_name: game.process_name.as_deref(),
            watch_only: false,
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
        Ok(json!({ "success": true }))
    }
}

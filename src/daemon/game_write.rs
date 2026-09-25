//! 写入口:新建一条档案与按字段改一条档案。daemon 是配置的唯一写者,这两个 RPC 是
//! UI 落盘的唯一两条路(扫描那条在 [`super::game_rpc`] 的 `game.add` 里)。

use super::protocol::{GamePatch, NewGame};
use super::*;

impl Daemon {
    /// Create a library entry from explicit user input (the manual add path —
    /// no scanning heuristics involved).
    pub(super) async fn rpc_game_create(&self, new_game: NewGame) -> Result<Value, String> {
        let name = new_game.name.trim().to_string();
        if name.is_empty() {
            return Err("名称不能为空".to_string());
        }
        for mount in [&new_game.game_dir_mount, &new_game.exe_mount]
            .into_iter()
            .flatten()
        {
            mount.validate()?;
        }
        // 带了挂载引用就**不要求路径此刻存在**:盘可能插在别的机器上、也可能还没插
        // (用户 2026-09-25:"大不了就是报错打不开,这是正常的")。没有引用时照旧要求
        // 它在 —— 否则一个拼错的路径会静悄悄建出一条永远打不开的档案。
        if new_game.exe_mount.is_none() && !new_game.exe_path.is_file() {
            return Err(format!("可执行文件不存在: {}", new_game.exe_path.display()));
        }

        let game_dir = match new_game.game_dir.clone() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                if new_game.game_dir_mount.is_none() && !dir.is_dir() {
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

        self.mutate_config(move |config| {
            // ⚠ **同一个 exe 只许有一条档案**(用户 2026-09-20 改的主意:从前是
            // 「警告但不阻止」,他后来认定它会给云同步留下说不清的坑 —— 版本历史
            // 按档案分开存、两条档案观测同一个进程。"能保证不出问题"之前,挡住更省事)。
            let owner = crate::game::exe_owner(config, &new_game.exe_path, None);
            if let Some(owner) = owner {
                return Err(format!(
                    "可执行文件已经属于「{owner}」：同一个 exe 只能建一条档案\
                     （两条档案会让云端的版本历史分家、观测同一个进程时分不清谁在跑）"
                ));
            }
            // 同名冲突在 `generate_unique_game_id` 里已经用后缀解决了 —— 同一款游戏
            // 改名建两条是合法需求,报错只会把它挡在门外。id 在写锁内生成,两个并发
            // create 不会抢到同一个。
            let id = crate::game::generate_unique_game_id(config, &name);
            if id.is_empty() {
                return Err("这个名称无法生成合法的游戏 ID，请换一个".to_string());
            }
            // 用户"把路径交上来"的这一刻顺手捕获一次（用户 2026-09-25 定的：
            // **捕获只发生一次**，就绑在这个动作上）；他自己填了引用就以他填的为准。
            let table = crate::mount::MountTable::read();
            let game_dir_mount = new_game
                .game_dir_mount
                .clone()
                .or_else(|| table.infer(&game_dir));
            let exe_mount = new_game
                .exe_mount
                .clone()
                .or_else(|| table.infer(&new_game.exe_path));
            // 引用是唯一主存储：命中引用之后绝对路径就不参与了（只是个占位符）。
            let stored_dir = if game_dir_mount.is_some() {
                PathBuf::new()
            } else {
                game_dir.clone()
            };
            let stored_exe = if exe_mount.is_some() {
                PathBuf::new()
            } else {
                new_game.exe_path.clone()
            };
            config.games.insert(
                id.clone(),
                crate::config::GameConfig {
                    cloud_id: None,
                    game_dir_mount,
                    exe_mount,
                    // 指纹与 `game::game_entry`（扫描那条路）同一个算法：见
                    // `sync::fingerprint`。读不到（盘不在）就是 `None`，等配对扫描
                    // 或者第一次上传时再补（`fill_fingerprints`）。
                    exe_fingerprint: crate::sync::fingerprint::of_file(&new_game.exe_path),
                    cloud_dir: None,
                    cloud_rejected: Vec::new(),
                    sync_enabled: true,
                    cloud_conclusion: None,
                    name: name.clone(),
                    game_dir: stored_dir,
                    exe_path: stored_exe,
                    launch_args: Vec::new(),
                    save_paths: Vec::new(),
                    wine_prefix: None,
                    // 默认开着自动追踪:用户自己起来的那一局也不漏(见 `GameConfig::auto_watch`)。
                    auto_watch: true,
                    direct_launch: false,
                    process_name: None,
                    scale_profile: crate::config::ScaleProfile::default_for(),
                    created_at: chrono::Utc::now(),
                },
            );
            tracing::info!("game.create: {id}");
            Ok(json!({ "id": id, "name": name }))
        })
        .await
    }

    /// Patch the mutable fields of a game. This is the only way a client
    /// persists game settings (the daemon is the single writer of the config).
    pub(super) async fn rpc_game_update(
        &self,
        id: &str,
        mut patch: GamePatch,
    ) -> Result<Value, String> {
        self.mutate_config(|config| {
            // Save paths are validated by resolving them, which needs an
            // immutable view of the game *and* the config; take that before
            // mutating anything.
            if let Some(save_paths) = &mut patch.save_paths {
                let snapshot = config
                    .games
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("配置中找不到游戏: {id}"))?;
                let mut candidate = snapshot;
                candidate.reconcile_save_paths(save_paths);
                if let Some(dir) = &patch.game_dir {
                    candidate.game_dir = dir.clone();
                    if !dir.as_os_str().is_empty() {
                        candidate.game_dir_mount = None;
                    }
                }
                // 同一包里显式给的引用也要在快照上生效，否则"只改引用、不改路径"
                // 那一次会拿旧引用去解析存档位置。
                if let Some(mount) = &patch.game_dir_mount {
                    candidate.game_dir_mount = mount.clone();
                }
                if let Some(mount) = &patch.exe_mount {
                    candidate.exe_mount = mount.clone();
                }
                let (root, _) = crate::wine::SaveRoot::for_platform(&candidate, config);
                // 有挂载引用时按它解析：盘没挂载就明确报错，而不是用一个假目录骗过验证。
                let game_dir = candidate.resolved_game_dir()?;
                for save in save_paths {
                    if save.mount.is_some() {
                        continue;
                    }
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

            // ⚠ 下面这一族的口径（用户 2026-09-25 定）：**引用是唯一主存储**，命中
            // 引用之后绝对路径就清空（它只是个占位符，绝不拿去解析）；而**自动捕获
            // 只发生在"用户把路径交上来"的这一刻** —— 改了路径就顺手识别一次盘，
            // 用户清空过的引用不会被后台重新填回来（见 `Daemon::run` 里的注释）。
            //
            // `has_reference` = 这一栏里**真的给了一个引用**：那种情况下路径不必存在，
            // 用户说什么就是什么。`stated` = 用户对这一栏**表过态**（给了引用，或者用
            // `null` 明说"不用了"）—— 表过态就不再自动捕获，否则用户亲手清掉的引用会
            // 在同一笔保存里被重新填回来。

            if let Some(dir) = &patch.game_dir {
                let stated = patch.game_dir_mount.is_some();
                let has_reference = matches!(patch.game_dir_mount, Some(Some(_)));
                if dir.as_os_str().is_empty() && !has_reference && game.game_dir_mount.is_none() {
                    return Err("游戏目录和挂载引用至少需要填写一项".into());
                }
                if !dir.as_os_str().is_empty() && !has_reference && !dir.is_dir() {
                    return Err(format!("游戏目录不存在: {}", dir.display()));
                }
                game.game_dir = dir.clone();
                if !dir.as_os_str().is_empty() {
                    game.game_dir_mount = None;
                    if !stated {
                        game.game_dir_mount = crate::mount::MountTable::read().infer(dir);
                    }
                    if game.game_dir_mount.is_some() {
                        game.game_dir = PathBuf::new();
                    }
                }
            }

            if let Some(exe) = &patch.exe_path {
                let stated = patch.exe_mount.is_some();
                let has_reference = matches!(patch.exe_mount, Some(Some(_)));
                if !exe.is_file() && !has_reference {
                    return Err(format!("可执行文件不存在: {}", exe.display()));
                }
                // exe 是这一款要跑的那个可执行文件 —— 指纹的三个时刻之一，**每次用户
                // 交上来都重算**（路径没变也要算：就地换了版本、打了补丁都该认出来）。
                // 算不出就如实记成"还不知道"，**不留上一次的答案** —— 一个过期的指纹
                // 会把这一款绑到别的云端身份上，那是唯一不可逆的错误。
                //
                // `cloud_id` / `cloud_dir` 一个字都不动：身份是"这一款在云端是谁"，
                // 粘住的，只有用户能改（配对界面）。换了版本也还是同一款游戏。
                game.exe_fingerprint = crate::sync::fingerprint::of_file(exe);
                game.exe_path = exe.clone();
                game.exe_mount = None;
                if !stated {
                    game.exe_mount = crate::mount::MountTable::read().infer(exe);
                }
                if game.exe_mount.is_some() {
                    game.exe_path = PathBuf::new();
                }
            }

            // 显式给的引用放在路径之后：同一包里两者都给时，引用赢（用户就是那么填的）。
            if let Some(mount) = &patch.game_dir_mount {
                match mount {
                    Some(mount) => {
                        mount.validate()?;
                        game.game_dir_mount = Some(mount.clone());
                        game.game_dir = PathBuf::new();
                    }
                    None => game.game_dir_mount = None,
                }
            }

            if let Some(mount) = &patch.exe_mount {
                match mount {
                    Some(mount) => {
                        mount.validate()?;
                        game.exe_mount = Some(mount.clone());
                        game.exe_path = PathBuf::new();
                        // 盘此刻在的话顺手把指纹补上；不在就如实记成"还不知道"。
                        game.exe_fingerprint = mount
                            .resolve()
                            .ok()
                            .and_then(|exe| crate::sync::fingerprint::of_file(&exe));
                    }
                    None => game.exe_mount = None,
                }
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

            if let Some(auto_watch) = patch.auto_watch {
                game.auto_watch = auto_watch;
            }

            // 每款一个的云同步开关（见 `GameConfig::sync_enabled`）。
            if let Some(sync_enabled) = patch.sync_enabled {
                game.sync_enabled = sync_enabled;
                // **手动重新打开 = 重新开始**（用户 2026-09-24："之后我不论开关云同步都不会
                // 再次弹窗，这也是问题"）：把上次那份结论（已确认 / 已拒绝）清掉，下一次启动
                // 会重新自检 —— 指纹还认得出就静默认领，认不出才再问一次。
                if sync_enabled {
                    game.cloud_conclusion = None;
                }
            }

            if let Some(direct_launch) = patch.direct_launch {
                game.direct_launch = direct_launch;
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
}

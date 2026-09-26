//! 一局游戏起来之后,顺手把「游戏分辨率」补上 —— **只补空着的那一栏**。
//!
//! 用户 2026-09-26 定的规则:
//!
//! * 档案里**已经填了**就一律不动:那是用户自己说的,探测没有资格改它;
//! * 空着才填,填的是 gamescope 报的**游戏窗口尺寸**(读法见 `scale::x11`);
//! * 探不到(超时、游戏已经退出、gamescope 没起来)什么都不做,档案保持原样;
//! * **本次启动不生效**:命令行早发出去了,这个数留给下一次启动用。
//!
//! 为什么落在 daemon 而不是 engine:写档案只有 daemon 会做(唯一写者,走
//! [`Daemon::mutate_config`]),而"读窗口"只有 engine 会做。两边各自守着自己的规矩,
//! 谁都不越界去干对方的活。

use super::*;
use crate::scale::ScaleSession;
use serde_json::json;

impl Daemon {
    /// 起一个后台任务,去探测这一局的游戏分辨率,探到就补进档案。
    ///
    /// 任务**不阻塞启动**:`game.launch` 该多快还多快,探测慢(最多 30 秒)是它自己的
    /// 事。`game_id` 与 `session` 都是这份档案在这一刻的样子 —— 会话结束时 engine 会
    /// 把 session 摘掉,探测跟着收工(见 `probe_game_resolution`)。
    pub(super) fn spawn_resolution_probe(&self, game_id: &str, session: ScaleSession) {
        let this = self.clone_shares();
        let game_id = game_id.to_string();

        tokio::spawn(async move {
            let Some(size) = this.engine.probe_game_resolution(&session).await else {
                // 探不到是常事(窗口还没画出来、这一局已经结束),不值得惊动用户:
                // 档案保持原样,下一次启动还有机会。
                tracing::debug!("{game_id}: 这次没探到游戏自己的分辨率，档案保持原样");
                return;
            };

            let id = game_id.clone();
            let written = this
                .mutate_config(move |config| {
                    let Some(game) = config.games.get_mut(&id) else {
                        // 探测这几十秒里用户把这一款删了 —— 没什么可填的。
                        return Ok(json!({ "written": false }));
                    };
                    // ⚠ 再查一遍:探测期间用户完全可能自己把这一栏填上了,那时**用户
                    // 的值说了算**,探测出来的那个不许覆盖它。
                    if game.scale_profile.explicit_internal_size().is_some() {
                        return Ok(json!({ "written": false }));
                    }
                    game.scale_profile.internal_width = Some(size.0);
                    game.scale_profile.internal_height = Some(size.1);
                    Ok(json!({ "written": true }))
                })
                .await;

            match written {
                Ok(value) if value["written"] == json!(true) => tracing::info!(
                    "{game_id}: 探到游戏自己画的是 {}x{}，已填进档案（下次启动生效）",
                    size.0,
                    size.1
                ),
                Ok(_) => tracing::debug!("{game_id}: 档案里已经有游戏分辨率，不动它"),
                Err(error) => tracing::warn!("{game_id}: 游戏分辨率没能写进档案：{error}"),
            }
        });
    }
}

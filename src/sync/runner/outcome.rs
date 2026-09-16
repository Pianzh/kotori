//! 一次同步操作的汇报类型：每个存档位置一条，整个游戏一条。
//!
//! 单独成文件，是因为 UI、CLI、daemon 的日志都直接照着这些字段说话——
//! "哪个位置、在哪、做了什么、为什么"，每一条都要能如实回答，`ok` 才敢说出口。

use serde::Serialize;

/// What happened to one save location.
#[derive(Debug, Clone, Serialize)]
pub struct LocationOutcome {
    /// The location as configured (portable form).
    pub configured: String,
    /// Where it resolved to on this machine.
    pub local: String,
    /// `uploaded` / `pulled` / `kept` / `restored` / `skipped` / `failed`.
    pub action: &'static str,
    /// One line explaining the action, safe to show to the user.
    pub detail: String,
}

impl LocationOutcome {
    pub(super) fn new(
        target: &crate::sync::SaveTarget,
        action: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            configured: target.configured.clone(),
            local: target.local.to_string_lossy().to_string(),
            action,
            detail: detail.into(),
        }
    }

    pub fn ok(&self) -> bool {
        self.action != "failed"
    }
}

/// Result of one operation on one game.
#[derive(Debug, Clone, Serialize)]
pub struct GameOutcome {
    pub game_id: String,
    pub name: String,
    pub ok: bool,
    pub locations: Vec<LocationOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl GameOutcome {
    pub fn failed(game_id: &str, name: &str, error: impl Into<String>) -> Self {
        Self {
            game_id: game_id.to_string(),
            name: name.to_string(),
            ok: false,
            locations: Vec::new(),
            error: Some(error.into()),
        }
    }

    pub(super) fn from_locations(
        game_id: &str,
        name: &str,
        locations: Vec<LocationOutcome>,
    ) -> Self {
        let error = locations
            .iter()
            .find(|o| !o.ok())
            .map(|o| format!("{}: {}", o.configured, o.detail));
        Self {
            game_id: game_id.to_string(),
            name: name.to_string(),
            ok: error.is_none(),
            locations,
            error,
        }
    }
}

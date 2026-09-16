//! 游戏与它的草稿:库里的一份游戏(`UiGame`)、编辑器里的草稿(`Draft`)、一笔在路上的
//! 自动保存(`SaveAttempt`),外加草稿用到的常量与存档位置类型。
//!
//! 「打开 -> 改 -> 自动保存」是一条完整生命周期,这几个类型住在一起:谁动草稿,
//! 谁就同时要看这三样。云同步那份状态在 `sync` 里,不在这里。

use crate::ui::*;

/// 单游戏设置页的自动保存:最后一次编辑之后等这么久才真的去写。
///
/// 700ms 是"手感上仍然算即时"与"打一串字只写一次"之间的折中。改一下存一次的那套
/// 见 `App::schedule_auto_save` 与 [`SaveAttempt`]。
pub(in crate::ui) const AUTOSAVE_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(700);

/// The three save-location kinds, as shown in the editor.
pub(in crate::ui) const SAVE_PATH_KINDS: [&str; 3] = ["windows", "relative", "absolute"];

/// One save location in the editor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavePathDraft {
    /// `windows` | `relative` | `absolute`.
    pub kind: String,
    pub path: String,
    /// Comma-separated glob patterns.
    pub exclude: String,
}

#[derive(Debug, Clone)]
pub struct UiGame {
    pub id: String,
    pub name: String,
    pub game_dir: String,
    pub exe: String,
    /// 传给这个 exe 的额外参数(存的是原始 argv,页面里拼成一行显示)。
    pub launch_args: Vec<String>,
    pub save_paths: Vec<SavePathDraft>,
    /// Watch-only games are started by the user; kotori follows the process.
    pub watch_only: bool,
    pub process_name: String,
    /// Profile name as stored, so saving never silently renames it.
    pub profile_name: String,
    pub algo: String,
    pub sharpness: u32,
    /// The game's own render resolution, or `None` on either half for "let
    /// gamescope decide" — which is what an empty field means now (nobody ever
    /// probed it, and the 1280x720 that used to live here was gamescope's own
    /// default written down).
    pub internal: (Option<u32>, Option<u32>),
    /// Explicit window size, or `None` on either half for "work it out at launch" —
    /// which is what an empty field in the advanced section means.
    pub output: (Option<u32>, Option<u32>),
    /// Scaling ratio as stored; `None` means the window opens at the screen's size
    /// (see `ScaleProfile::output_size_for`).
    pub scale_ratio: Option<f32>,
    pub fullscreen: bool,
    pub framerate: Option<u32>,
    /// 用户手写的 gamescope 参数;非空时它取代档案里的一切(见
    /// [`ScaleProfile::gamescope_args`])。
    pub gamescope_args: Vec<String>,
}

/// Editable copy of a game's scale profile.
#[derive(Debug, Clone)]
pub(in crate::ui) struct Draft {
    pub(in crate::ui) game_id: String,
    pub(in crate::ui) profile_name: String,
    /// Editable game root and exe path, plus their stored values so unchanged
    /// fields are not re-sent (the daemon rejects a path that does not exist,
    /// e.g. when the game lives on a drive that is not mounted right now).
    pub(in crate::ui) game_dir: String,
    pub(in crate::ui) game_dir_original: String,
    pub(in crate::ui) exe: String,
    pub(in crate::ui) exe_original: String,
    /// exe 的额外参数。跟 exe 路径一样留着原值:没改就不发,免得打断一次
    /// 正在跑的游戏(daemon 那边是按字段 patch 的)。
    pub(in crate::ui) launch_args: String,
    pub(in crate::ui) launch_args_original: String,
    pub(in crate::ui) save_paths: Vec<SavePathDraft>,
    pub(in crate::ui) save_paths_original: Vec<SavePathDraft>,
    pub(in crate::ui) algo: String,
    pub(in crate::ui) sharpness: u32,
    pub(in crate::ui) internal_w: String,
    pub(in crate::ui) internal_h: String,
    pub(in crate::ui) output_w: String,
    pub(in crate::ui) output_h: String,
    /// Kept as text so a half-typed ratio survives an edit. Empty means "no
    /// ratio". The widget for it arrives with the scale-section rework; until
    /// then this only carries the stored value through an open + save.
    pub(in crate::ui) scale_ratio: String,
    pub(in crate::ui) fullscreen: bool,
    pub(in crate::ui) framerate: String,
    /// gamescope 自由参数,存成**一行文本**:页面上就是这么编辑的,存成 argv
    /// 每次都要切分再拼回来,而且用户写的原样(多余空格)会丢。
    pub(in crate::ui) gamescope_args: String,
}

impl Draft {
    /// Seed the form from the *stored* profile. Anything else means a plain
    /// "open + save" silently rewrites the user's settings.
    pub(in crate::ui) fn from_game(game: &UiGame) -> Self {
        Self {
            game_id: game.id.clone(),
            profile_name: game.profile_name.clone(),
            game_dir: game.game_dir.clone(),
            game_dir_original: game.game_dir.clone(),
            exe: game.exe.clone(),
            exe_original: game.exe.clone(),
            launch_args: game.launch_args.join(" "),
            launch_args_original: game.launch_args.join(" "),
            save_paths: game.save_paths.clone(),
            save_paths_original: game.save_paths.clone(),
            algo: if ScaleAlgorithm::ALL.contains(&game.algo.as_str()) {
                game.algo.clone()
            } else {
                ScaleAlgorithm::Fsr {
                    sharpness: game.sharpness,
                }
                .label()
                .to_string()
            },
            sharpness: game.sharpness,
            internal_w: game.internal.0.map(|v| v.to_string()).unwrap_or_default(),
            internal_h: game.internal.1.map(|v| v.to_string()).unwrap_or_default(),
            output_w: game.output.0.map(|v| v.to_string()).unwrap_or_default(),
            output_h: game.output.1.map(|v| v.to_string()).unwrap_or_default(),
            scale_ratio: game.scale_ratio.map(|r| r.to_string()).unwrap_or_default(),
            fullscreen: game.fullscreen,
            framerate: game.framerate.map(|f| f.to_string()).unwrap_or_default(),
            gamescope_args: game.gamescope_args.join(" "),
        }
    }

    /// Has the user changed the exe path?
    pub(in crate::ui) fn exe_changed(&self) -> bool {
        self.exe.trim() != self.exe_original
    }

    /// Has the user changed the game root?
    pub(in crate::ui) fn game_dir_changed(&self) -> bool {
        self.game_dir.trim() != self.game_dir_original
    }

    /// Has the user changed the exe's extra arguments?
    pub(in crate::ui) fn launch_args_changed(&self) -> bool {
        self.launch_args.trim() != self.launch_args_original.trim()
    }

    /// Has the user changed the save locations?
    pub(in crate::ui) fn save_paths_changed(&self) -> bool {
        self.save_paths != self.save_paths_original
    }

    /// 页面上看得见的那些值,是否已经和「已存值」一样。
    ///
    /// 比较时**故意忽略 `*_original`**:它们是"服务端有什么"的书签,不是页面内容。
    /// 档案本身直接比(`ScaleProfile` 有 `PartialEq`),路径按去空白后的文本比 ——
    /// 这样"重置"能如实回答"有没有东西可还原"。
    pub(in crate::ui) fn matches_stored(&self, game: &UiGame) -> bool {
        let stored = Draft::from_game(game);
        self.exe.trim() == stored.exe.trim()
            && self.game_dir.trim() == stored.game_dir.trim()
            && self.launch_args.trim() == stored.launch_args.trim()
            && self.save_paths == stored.save_paths
            && profile_from_draft(self).ok() == profile_from_draft(&stored).ok()
    }
}

/// 一笔在路上的自动保存:带走了哪份草稿(以及它属于哪个游戏)。
///
/// 成功之后要把 `*_original` 推进到**带走的这份**上(不是手上这份 —— 用户可能又改过
/// 了):它代表"服务端现在有的值",下一次自动保存据此只发改过的字段。少推进这一下,
/// 游戏盘一没挂载就会连"改个锐度"都存不进去。
#[derive(Debug, Clone)]
pub(in crate::ui) struct SaveAttempt {
    pub(in crate::ui) draft: Draft,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::ui_game;

    #[test]
    fn draft_seeds_exe_and_detects_changes() {
        let game = ui_game();
        let mut draft = Draft::from_game(&game);
        assert_eq!(draft.exe, game.exe);
        assert!(
            !draft.exe_changed(),
            "opening a game must not count as an edit"
        );

        draft.exe = "/games/demo/other.exe".into();
        assert!(draft.exe_changed());

        // Whitespace-only differences are not an edit either.
        let mut padded = Draft::from_game(&game);
        padded.exe = format!("  {}  ", game.exe);
        assert!(!padded.exe_changed());
    }
}

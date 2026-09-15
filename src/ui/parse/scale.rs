//! 缩放档案:编辑器里的草稿 -> `ScaleProfile`,以及从回包里认算法标签/锐度。
//!
//! 输入是页面上那堆文本框,不是 daemon 的 JSON —— 所以和 `games` 分开:谁动
//! 缩放算法、分辨率、帧率,只动这一个文件。

use super::*;

pub(in crate::ui) fn profile_from_draft(draft: &Draft) -> Result<ScaleProfile, String> {
    let algorithm = ScaleAlgorithm::from_label(&draft.algo)
        .ok_or_else(|| format!("未知缩放算法: {}", draft.algo))?
        .with_sharpness(draft.sharpness);

    // 留空＝自动(启动时按屏幕算);填了才覆盖。半截数字是用户能改的错误,
    // 不是"静默退回自动" —— 那等于把他刚填的东西悄悄丢掉。
    let scale_ratio = match draft.scale_ratio.trim() {
        "" => None,
        raw => Some(
            raw.parse::<f32>()
                .map_err(|_| format!("缩放比例必须是数字（当前 {raw}）"))?,
        ),
    };

    Ok(ScaleProfile {
        name: draft.profile_name.clone(),
        algorithm,
        internal_width: parse_optional_u32(&draft.internal_w, "游戏分辨率宽")?,
        internal_height: parse_optional_u32(&draft.internal_h, "游戏分辨率高")?,
        output_width: parse_optional_u32(&draft.output_w, "输出分辨率宽")?,
        output_height: parse_optional_u32(&draft.output_h, "输出分辨率高")?,
        scale_ratio,
        follow_window: draft.follow_window,
        framerate_limit: if draft.framerate.trim().is_empty() {
            None
        } else {
            Some(parse_u32(&draft.framerate, "帧率限制")?)
        },
        force_fullscreen: draft.fullscreen,
    })
}

pub(in crate::ui) fn parse_u32(s: &str, label: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} 必须是正整数"))
}

/// 留空的数字字段＝"不说"(＝自动),不是 0。
pub(in crate::ui) fn parse_optional_u32(s: &str, label: &str) -> Result<Option<u32>, String> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    parse_u32(s, label).map(Some)
}

/// Algorithm label from the serialized form. Serde tags struct variants as an
/// object (`{"Fsr": {"sharpness": 2}}`) but unit variants as a bare string
/// (`"Integer"`), so both shapes must be handled.
pub(in crate::ui) fn algo_label(v: &Value) -> Option<String> {
    if let Some(label) = v.as_str() {
        return Some(label.to_string());
    }
    let obj = v.as_object()?;
    obj.keys().next().cloned()
}

/// `sharpness` of the serialized algorithm, when it has one.
pub(in crate::ui) fn algo_sharpness(v: &Value) -> Option<u32> {
    let obj = v.as_object()?;
    let (_k, val) = obj.iter().next()?;
    val.get("sharpness")?.as_u64().map(|s| s as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::ui_game;

    fn draft_with(algo: &str) -> Draft {
        Draft {
            game_id: "x".into(),
            profile_name: "默认".into(),
            game_dir: "/games/x".into(),
            game_dir_original: "/games/x".into(),
            exe: "/games/x/game.exe".into(),
            exe_original: "/games/x/game.exe".into(),
            save_paths: Vec::new(),
            save_paths_original: Vec::new(),
            algo: algo.into(),
            sharpness: 2,
            internal_w: "1280".into(),
            internal_h: "720".into(),
            output_w: "2560".into(),
            output_h: "1440".into(),
            scale_ratio: String::new(),
            follow_window: true,
            fullscreen: true,
            framerate: String::new(),
        }
    }

    #[test]
    fn unknown_algorithm_is_rejected_on_save() {
        assert!(profile_from_draft(&draft_with("Lanczos")).is_err());
    }

    #[test]
    fn non_numeric_resolution_is_rejected() {
        let mut draft = draft_with("Fsr");
        draft.internal_w = "abc".into();
        let err = profile_from_draft(&draft).unwrap_err();
        assert!(err.contains("游戏分辨率宽"), "{err}");
    }

    /// A plain open + save must neither invent nor drop a scaling ratio:
    /// "no ratio" (the state every older profile is in) stays "no ratio", a set
    /// one survives, and half-typed text is an error rather than a silent reset.
    #[test]
    fn the_scaling_ratio_survives_an_open_and_save() {
        let game = ui_game();
        let untouched = profile_from_draft(&Draft::from_game(&game)).unwrap();
        assert_eq!(untouched.scale_ratio, None);
        assert!(untouched.follow_window);

        let mut pinned = game.clone();
        pinned.scale_ratio = Some(1.5);
        pinned.follow_window = false;
        let profile = profile_from_draft(&Draft::from_game(&pinned)).unwrap();
        assert_eq!(profile.scale_ratio, Some(1.5));
        assert!(!profile.follow_window);

        let mut half_typed = Draft::from_game(&pinned);
        half_typed.scale_ratio = "1.5x".into();
        let err = profile_from_draft(&half_typed).unwrap_err();
        assert!(err.contains("缩放比例"), "{err}");
    }
}

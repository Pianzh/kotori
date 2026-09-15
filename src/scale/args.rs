//! gamescope's command line: what a launch looks like.
//!
//! The parameter model is version-sensitive and has burned us before (see the
//! notes on [`build_gamescope_args`]), so it lives in one file with its tests.
use crate::config::{MAX_SHARPNESS, ScaleAlgorithm, ScaleProfile};

/// Translate kotori's internal sharpness (0 = softest, 5 = sharpest) into
/// gamescope's `--sharpness`, whose scale is inverted (0 = max, 20 = min).
pub fn sharpness_to_gamescope(sharpness: u32) -> u32 {
    20 - sharpness.min(MAX_SHARPNESS) * 4
}

/// Build gamescope command line arguments from a scale profile.
///
/// targets gamescope >= 3.16 parameter model:
///   -w/-h :: game (nested) resolution
///   -W/-H :: output resolution
///   -S    :: scaler type (auto, integer, fit, fill, stretch)
///   -F    :: filter (linear, nearest, fsr, nis, pixel)
///   --sharpness :: 0 (max) .. 20 (min)
///
/// `-w/-h` are emitted **only when the profile names a game resolution**
/// (user's call, 2026-09-13: that field defaults to empty, because nobody ever
/// probed it and a stored 1280x720 was just gamescope's own default written
/// down). Omitting them is not "unknown": gamescope substitutes the very same
/// 1280x720, so a launch with no `-w/-h` draws exactly what one with
/// `-w 1280 -h 720` drew — which is why leaving it empty changes nothing on
/// screen and everything in the config.
///
/// `-s` is `--mouse-sensitivity` since 3.16 and must never be emitted here
/// (it swallows the following argument), and `--fsr-sharpness` is only an
/// alias of `--sharpness` that older code passed with the wrong polarity.
///
/// `-W/-H` are the *initial* output size in physical pixels, from
/// [`ScaleProfile::output_size_for`]: whatever the profile explicitly asks for,
/// otherwise `screen` — and "the screen" is exactly what "start maximised" means,
/// because these are only the *initial* size. gamescope treats them as a preferred
/// size only: a nested window stays freely resizable and gamescope follows every
/// resize by adopting the new content size as its output size, so there is no
/// flag here (and none to invent) that would pin the window's size.
///
/// `screen` is passed in rather than probed here, so this stays a pure function of
/// its arguments and no test depends on the machine it runs on.
pub fn build_gamescope_args(
    profile: &ScaleProfile,
    screen: (u32, u32),
    game_cmd: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    if let Some((width, height)) = profile.explicit_internal_size() {
        args.extend([
            "-w".into(),
            width.to_string(),
            "-h".into(),
            height.to_string(),
        ]);
    }

    let (output_width, output_height) = profile.output_size_for(screen);
    args.extend([
        "-W".into(),
        output_width.to_string(),
        "-H".into(),
        output_height.to_string(),
    ]);

    // Scale algorithm -> scaler/filter/sharpness.
    match &profile.algorithm {
        ScaleAlgorithm::Fsr { sharpness } => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("fsr".into());
            args.push("--sharpness".into());
            args.push(sharpness_to_gamescope(*sharpness).to_string());
        }
        ScaleAlgorithm::Nis { sharpness } => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("nis".into());
            args.push("--sharpness".into());
            args.push(sharpness_to_gamescope(*sharpness).to_string());
        }
        ScaleAlgorithm::Integer => {
            args.push("-S".into());
            args.push("integer".into());
            args.push("-F".into());
            args.push("nearest".into());
        }
        ScaleAlgorithm::Bilinear => {
            args.push("-S".into());
            args.push("fit".into());
            args.push("-F".into());
            args.push("linear".into());
        }
    }

    if let Some(fps) = profile.framerate_limit {
        args.push("-r".into());
        args.push(fps.to_string());
    }

    if profile.force_fullscreen {
        args.push("-f".into());
    }

    // Separator
    args.push("--".into());

    // Game command
    args.extend(game_cmd.iter().cloned());

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScaleAlgorithm;

    fn profile(algorithm: ScaleAlgorithm) -> ScaleProfile {
        ScaleProfile {
            algorithm,
            framerate_limit: None,
            force_fullscreen: false,
            ..ScaleProfile::default_for()
        }
    }

    /// The display every test pretends to be on. Passed in explicitly, so these
    /// tests say nothing about the machine they happen to run on.
    const SCREEN: (u32, u32) = (2560, 1440);

    fn game_cmd() -> Vec<String> {
        vec!["/usr/bin/wine".into(), "/games/x/game.exe".into()]
    }

    /// Index of a flag, or panic with a readable message.
    fn index_of(args: &[String], flag: &str) -> usize {
        args.iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing from {args:?}"))
    }

    /// 两样都留空时,窗口就开在屏幕上 —— `-W/-H` 等于屏幕分辨率(＝启动即最大化),
    /// 而游戏分辨率根本不发(用户 2026-09-13:那一项默认就是空的)。
    #[test]
    fn an_empty_profile_opens_at_the_screen_size() {
        let p = profile(ScaleAlgorithm::Integer);
        assert_eq!(p.explicit_output_size(), None);
        let args = build_gamescope_args(&p, SCREEN, &game_cmd());
        assert_eq!(&args[..4], ["-W", "2560", "-H", "1440"]);
        assert!(
            !args.iter().any(|a| a == "-w" || a == "-h"),
            "游戏分辨率留空时不该发 -w/-h（gamescope 自己的默认值就是它）:{args:?}"
        );
    }

    /// 填了游戏分辨率才发 `-w/-h` —— 这是它们唯一的存在理由。
    #[test]
    fn only_an_explicit_game_resolution_emits_w_and_h() {
        let mut p = profile(ScaleAlgorithm::Integer);
        p.internal_width = Some(640);
        // 半截数字不算数:宁可一对都不发,也不要发出一个瞎猜的另一半。
        assert_eq!(p.explicit_internal_size(), None);
        assert!(
            !build_gamescope_args(&p, SCREEN, &game_cmd())
                .iter()
                .any(|a| a == "-w")
        );

        p.internal_height = Some(480);
        let args = build_gamescope_args(&p, SCREEN, &game_cmd());
        assert_eq!(
            &args[..8],
            ["-w", "640", "-h", "480", "-W", "2560", "-H", "1440"]
        );
    }

    #[test]
    fn a_scaling_ratio_wins_over_an_explicit_size() {
        let mut p = profile(ScaleAlgorithm::Fsr { sharpness: 2 });
        p.internal_width = Some(1280);
        p.internal_height = Some(720);
        p.output_width = Some(2560);
        p.output_height = Some(1440);
        p.scale_ratio = Some(1.5);
        let args = build_gamescope_args(&p, SCREEN, &game_cmd());
        // 1280x720 * 1.5,不是填进去的 2560x1440。
        assert_eq!(
            &args[..8],
            ["-w", "1280", "-h", "720", "-W", "1920", "-H", "1080"]
        );
    }

    /// 高级设置里填了尺寸就以它为准 —— 不填才按屏幕。
    #[test]
    fn an_explicit_size_overrides_the_screen() {
        let mut p = profile(ScaleAlgorithm::Integer);
        p.output_width = Some(1600);
        p.output_height = Some(900);
        assert_eq!(p.scale_ratio, None);
        let args = build_gamescope_args(&p, SCREEN, &game_cmd());
        assert_eq!(&args[..4], ["-W", "1600", "-H", "900"]);
    }

    #[test]
    fn fsr_uses_the_316_scaler_model() {
        let args = build_gamescope_args(
            &profile(ScaleAlgorithm::Fsr { sharpness: 2 }),
            SCREEN,
            &game_cmd(),
        );
        assert_eq!(args[index_of(&args, "-S") + 1], "fit");
        assert_eq!(args[index_of(&args, "-F") + 1], "fsr");
        assert_eq!(args[index_of(&args, "--sharpness") + 1], "12");
    }

    #[test]
    fn never_emits_the_legacy_mouse_sensitivity_flag() {
        // `-s` used to mean FSR and now eats the next argument; `--fsr-sharpness`
        // had inverted polarity. Neither may come back.
        for algorithm in [
            ScaleAlgorithm::Fsr { sharpness: 3 },
            ScaleAlgorithm::Nis { sharpness: 3 },
            ScaleAlgorithm::Integer,
            ScaleAlgorithm::Bilinear,
        ] {
            let args = build_gamescope_args(&profile(algorithm), SCREEN, &game_cmd());
            assert!(!args.iter().any(|a| a == "-s"), "legacy -s in {args:?}");
            assert!(
                !args.iter().any(|a| a == "--fsr-sharpness"),
                "legacy --fsr-sharpness in {args:?}"
            );
        }
    }

    #[test]
    fn sharpness_polarity_matches_gamescope() {
        assert_eq!(sharpness_to_gamescope(0), 20);
        assert_eq!(sharpness_to_gamescope(2), 12);
        assert_eq!(sharpness_to_gamescope(5), 0);
        // Out-of-range values are clamped, never negative.
        assert_eq!(sharpness_to_gamescope(99), 0);
    }

    #[test]
    fn nis_and_bilinear_pick_their_filters() {
        let nis = build_gamescope_args(
            &profile(ScaleAlgorithm::Nis { sharpness: 5 }),
            SCREEN,
            &game_cmd(),
        );
        assert_eq!(nis[index_of(&nis, "-F") + 1], "nis");
        assert_eq!(nis[index_of(&nis, "--sharpness") + 1], "0");

        let bilinear =
            build_gamescope_args(&profile(ScaleAlgorithm::Bilinear), SCREEN, &game_cmd());
        assert_eq!(bilinear[index_of(&bilinear, "-F") + 1], "linear");
        assert!(!bilinear.iter().any(|a| a == "--sharpness"));
    }

    #[test]
    fn integer_scaling_uses_nearest_neighbour() {
        let args = build_gamescope_args(&profile(ScaleAlgorithm::Integer), SCREEN, &game_cmd());
        assert_eq!(args[index_of(&args, "-S") + 1], "integer");
        assert_eq!(args[index_of(&args, "-F") + 1], "nearest");
    }

    #[test]
    fn optional_flags_are_only_emitted_when_requested() {
        let bare = build_gamescope_args(&profile(ScaleAlgorithm::Integer), SCREEN, &game_cmd());
        assert!(!bare.iter().any(|a| a == "-r"));
        assert!(!bare.iter().any(|a| a == "-f"));

        let mut p = profile(ScaleAlgorithm::Integer);
        p.framerate_limit = Some(60);
        p.force_fullscreen = true;
        let full = build_gamescope_args(&p, SCREEN, &game_cmd());
        assert_eq!(full[index_of(&full, "-r") + 1], "60");
        assert!(full.iter().any(|a| a == "-f"));
    }

    /// 新人默认拿到的必须是一个**能被拖小的普通窗口**,不是全屏(用户 2026-09-13 拍板)。
    ///
    /// `-f` 把 `g_nOutput` 钉成屏幕几何:配置的比例白设,用户自己拖窗口也没用 ——
    /// 那正是"只能缩不能放"的一半原因。默认开在屏幕大小、但不带 `-f`,就已经是
    /// 用户要的"最大化"了,而且还能自己缩小。
    #[test]
    fn a_brand_new_profile_launches_as_a_resizable_window() {
        let args = build_gamescope_args(&ScaleProfile::default_for(), SCREEN, &game_cmd());
        assert!(
            !args.iter().any(|a| a == "-f"),
            "默认不该带 -f,否则窗口在合成器眼里就不可缩放了：{args:?}"
        );
    }

    #[test]
    fn game_command_follows_the_separator_last() {
        let args = build_gamescope_args(&profile(ScaleAlgorithm::Integer), SCREEN, &game_cmd());
        let sep = index_of(&args, "--");
        assert_eq!(&args[sep + 1..], ["/usr/bin/wine", "/games/x/game.exe"]);
        // Nothing after the separator may look like a gamescope flag.
        assert_eq!(args.len(), sep + 3);
    }
}


use super::*;

#[test]
fn sharpness_is_clamped_and_rebuilt() {
    let algo = ScaleAlgorithm::Fsr { sharpness: 99 };
    assert_eq!(algo.sharpness(), Some(MAX_SHARPNESS));
    assert_eq!(
        ScaleAlgorithm::Integer.with_sharpness(3),
        ScaleAlgorithm::Integer
    );
    assert_eq!(
        ScaleAlgorithm::Nis { sharpness: 1 }.with_sharpness(5),
        ScaleAlgorithm::Nis { sharpness: 5 }
    );
}

#[test]
fn algorithm_labels_round_trip() {
    for label in ScaleAlgorithm::ALL {
        let algo = ScaleAlgorithm::from_label(label).expect(label);
        assert_eq!(algo.label(), label);
    }
    assert!(ScaleAlgorithm::from_label("Lanczos").is_none());
}

/// 留空＝自动(启动时按屏幕来);填了才覆盖。用户 2026-09-13 定的语义。
#[test]
fn an_empty_profile_gets_the_screen_and_a_filled_one_overrides_it() {
    let screen = (2560, 1440);
    let mut profile = ScaleProfile::default_for();

    // 两样都留空:窗口就开在屏幕上 —— 这就是"启动即最大化"。
    assert_eq!(profile.explicit_output_size(), None);
    assert_eq!(profile.output_size_for(screen), screen);

    // 输出分辨率只填一半不算数:半个尺寸不是尺寸,宁可忽略也不要瞎猜另一半。
    profile.output_width = Some(1600);
    assert_eq!(profile.explicit_output_size(), None);
    assert_eq!(profile.output_size_for(screen), screen);

    profile.output_height = Some(900);
    assert_eq!(profile.explicit_output_size(), Some((1600, 900)));
    assert_eq!(profile.output_size_for(screen), (1600, 900));

    // 倍数更具体,填了就以它为准。
    profile.scale_ratio = Some(1.5);
    assert_eq!(profile.explicit_output_size(), Some((1920, 1080)));

    // 小倍数会取整,但绝不塌成 0 尺寸窗口。
    profile.internal_width = Some(1000);
    profile.internal_height = Some(999);
    profile.scale_ratio = Some(0.25);
    assert_eq!(profile.explicit_output_size(), Some((250, 250)));

    // 不可用的倍数(0 / NaN)当没填,退回尺寸。
    profile.scale_ratio = Some(0.0);
    assert_eq!(profile.explicit_output_size(), Some((1600, 900)));
    profile.scale_ratio = Some(f32::NAN);
    assert_eq!(profile.explicit_output_size(), Some((1600, 900)));
    // ...而离谱的那个是夹取,不是溢出。
    profile.scale_ratio = Some(1e30);
    assert_eq!(
        profile.explicit_output_size(),
        Some((MAX_RESOLUTION, MAX_RESOLUTION))
    );
}

#[test]
fn profile_validation_bounds_the_scaling_ratio() {
    let mut profile = ScaleProfile::default_for();

    profile.scale_ratio = Some(MIN_SCALE_RATIO / 2.0);
    assert!(profile.validate().unwrap_err().contains("缩放比例"));
    profile.scale_ratio = Some(MAX_SCALE_RATIO + 1.0);
    assert!(profile.validate().is_err());
    profile.scale_ratio = Some(f32::NAN);
    assert!(profile.validate().is_err());
    profile.scale_ratio = Some(2.0);
    assert!(profile.validate().is_ok());

    // In range, but the product blows past the resolution ceiling: rejected
    // here rather than silently clamped by `output_size()`.
    profile.internal_width = Some(MAX_RESOLUTION);
    profile.internal_height = Some(MAX_RESOLUTION);
    profile.scale_ratio = Some(2.0);
    let err = profile.validate().unwrap_err();
    assert!(err.contains("输出分辨率"), "{err}");
}

/// 游戏分辨率留空是**正常状态**(用户 2026-09-13:不再让用户填也默认不填)。
///
/// 空的时候按 gamescope 自己的默认算数(`-w/-h` 根本不发),而落在配置里的
/// 那一对数字必须仍然是 `None` —— 不能偷偷把默认值写回档案。
#[test]
fn an_empty_game_resolution_leaves_the_size_to_gamescope() {
    let mut profile = ScaleProfile::default_for();
    assert_eq!(profile.explicit_internal_size(), None);
    assert_eq!(
        profile.internal_size(),
        (GAMESCOPE_DEFAULT_WIDTH, GAMESCOPE_DEFAULT_HEIGHT)
    );
    assert!(profile.validate().is_ok());

    // 只填一半不算数:半个分辨率不是分辨率。
    profile.internal_width = Some(640);
    assert_eq!(profile.explicit_internal_size(), None);
    assert_eq!(
        profile.internal_size(),
        (GAMESCOPE_DEFAULT_WIDTH, GAMESCOPE_DEFAULT_HEIGHT)
    );
    // 0 也不算数,而且要被拒(它就是"填错了")。
    profile.internal_width = Some(0);
    assert_eq!(profile.explicit_internal_size(), None);
    assert!(profile.validate().unwrap_err().contains("游戏分辨率宽"));

    profile.internal_width = Some(640);
    profile.internal_height = Some(480);
    assert_eq!(profile.explicit_internal_size(), Some((640, 480)));
    assert_eq!(profile.internal_size(), (640, 480));

    // 空着＋填了倍数:倍数按 gamescope 的默认分辨率算,而不是算不出来。
    let mut ratio = ScaleProfile::default_for();
    ratio.scale_ratio = Some(1.5);
    assert_eq!(ratio.explicit_output_size(), Some((1920, 1080)));
}

#[test]
fn profile_validation_rejects_impossible_values() {
    let ok = ScaleProfile::default_for();
    assert!(ok.validate().is_ok());

    let mut zero = ok.clone();
    zero.internal_width = Some(0);
    assert!(zero.validate().unwrap_err().contains("游戏分辨率宽"));

    let mut huge = ok.clone();
    huge.output_height = Some(MAX_RESOLUTION + 1);
    assert!(huge.validate().unwrap_err().contains("输出分辨率高"));

    // 留空是正常状态(＝自动),0 才是错的。
    let mut empty = ok.clone();
    empty.output_width = None;
    empty.output_height = None;
    assert!(empty.validate().is_ok());
    let mut zero_out = ok.clone();
    zero_out.output_width = Some(0);
    assert!(zero_out.validate().unwrap_err().contains("输出分辨率宽"));

    let mut fps = ok.clone();
    fps.framerate_limit = Some(0);
    assert!(fps.validate().unwrap_err().contains("帧率限制"));

    let mut fps_high = ok.clone();
    fps_high.framerate_limit = Some(MAX_FRAMERATE + 1);
    assert!(fps_high.validate().is_err());

    assert!(
        ok.validate().is_ok(),
        "validation must not mutate the profile"
    );
}

#[test]
fn normalize_clamps_sharpness_only() {
    let mut profile = ScaleProfile {
        algorithm: ScaleAlgorithm::Nis { sharpness: 99 },
        ..ScaleProfile::default_for()
    };
    profile.normalize();
    assert_eq!(
        profile.algorithm,
        ScaleAlgorithm::Nis {
            sharpness: MAX_SHARPNESS
        }
    );

    let mut unit = ScaleProfile {
        algorithm: ScaleAlgorithm::Integer,
        ..ScaleProfile::default_for()
    };
    unit.normalize();
    assert_eq!(unit.algorithm, ScaleAlgorithm::Integer);
}

/// 用户自己写 `--` 是这套功能里唯一"必然出错、又能提前拦住"的写法:kotori 自己
/// 要在游戏命令前放一个,再来一个 gamescope 就把后面半截当成要跑的命令 ——
/// 报出来的错跟真正的原因毫无关系。
#[test]
fn a_hand_written_separator_is_rejected() {
    let mut profile = ScaleProfile::default_for();
    profile.gamescope_args = vec!["-f".into(), "--".into(), "-W".into()];
    let err = profile.validate().unwrap_err();
    assert!(err.contains("--"), "{err}");

    // `--sharpness` 里也有两个连字符,它必须照常通过 —— 挡住它等于把这个功能
    // 最常用的一类参数(长选项)废掉。
    profile.gamescope_args = vec!["--sharpness".into(), "5".into()];
    assert!(profile.validate().is_ok(), "长选项不该被当成分隔符");
}

/// 自由参数是"写了才算数":空数组就是老行为(由 kotori 拼),不是"空的自由参数"。
#[test]
fn free_form_is_about_the_arguments_being_there() {
    let mut profile = ScaleProfile::default_for();
    assert!(!profile.free_form());

    profile.gamescope_args = vec!["-f".into()];
    assert!(profile.free_form());

    profile.gamescope_args = Vec::new();
    assert!(!profile.free_form(), "清空自定义参数就回到自动");
}

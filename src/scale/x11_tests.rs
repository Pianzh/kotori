//! `x11` 的单元测试：显示号、算法映射、窗口属性。
//!
//! 从 `x11.rs` 拆出来 —— 那边连着测试一起数越过了 500 行的硬线（AGENTS.md）。

use super::*;

fn fsr(sharpness: u32) -> ScaleAlgorithm {
    ScaleAlgorithm::Fsr { sharpness }
}

#[test]
fn runtime_settings_match_the_launch_arguments() {
    // -S fit -F fsr --sharpness (20 - internal * 4)
    let settings = Settings::for_algorithm(&fsr(2));
    assert_eq!(settings.filter, Filter::Fsr);
    assert_eq!(settings.scaler, Scaler::Fit);
    assert_eq!(settings.sharpness, 12);

    // -S integer -F nearest
    let settings = Settings::for_algorithm(&ScaleAlgorithm::Integer);
    assert_eq!(settings.filter, Filter::Nearest);
    assert_eq!(settings.scaler, Scaler::Integer);

    // -S fit -F linear
    let settings = Settings::for_algorithm(&ScaleAlgorithm::Bilinear);
    assert_eq!(settings.filter, Filter::Linear);
    assert_eq!(settings.scaler, Scaler::Fit);

    let settings = Settings::for_algorithm(&ScaleAlgorithm::Nis { sharpness: 5 });
    assert_eq!(settings.filter, Filter::Nis);
    assert_eq!(settings.scaler, Scaler::Fit);
    assert_eq!(settings.sharpness, 0);
}

#[test]
fn toggling_fsr_twice_comes_back_to_where_it_started() {
    let start = Settings::for_algorithm(&fsr(2));
    let off = start.toggled_fsr();
    assert_eq!(off.filter, Filter::Linear);
    assert_eq!(off.toggled_fsr().filter, Filter::Fsr);
    // Sharpness survives the round trip: turning FSR off and on again must not
    // silently reset the setting the user chose at launch.
    assert_eq!(off.toggled_fsr().sharpness, start.sharpness);
}

#[test]
fn toggling_nis_is_the_same_shape() {
    let start = Settings::for_algorithm(&fsr(2));
    assert_eq!(start.toggled_nis().filter, Filter::Nis);
    assert_eq!(start.toggled_nis().toggled_nis().filter, Filter::Linear);
}

#[test]
fn nearest_and_bilinear_pick_a_scaler_too() {
    let start = Settings::for_algorithm(&fsr(2));
    assert_eq!(start.nearest().scaler, Scaler::Integer);
    assert_eq!(start.nearest().filter, Filter::Nearest);
    assert_eq!(start.bilinear().scaler, Scaler::Fit);
    assert_eq!(start.bilinear().filter, Filter::Linear);
}

#[test]
fn sharpness_stops_at_gamescopes_limits() {
    // gamescope: 0 = sharpest, 20 = softest.
    let sharpest = Settings {
        sharpness: 0,
        ..Settings::for_algorithm(&fsr(0))
    };
    assert_eq!(sharpest.sharper().sharpness, 0);
    let softest = Settings {
        sharpness: GAMESCOPE_MAX_SHARPNESS,
        ..Settings::for_algorithm(&fsr(0))
    };
    assert_eq!(softest.softer().sharpness, GAMESCOPE_MAX_SHARPNESS);
    assert_eq!(softest.sharper().sharpness, GAMESCOPE_MAX_SHARPNESS - 1);
}

#[test]
fn unknown_filter_values_are_reported_not_guessed() {
    assert!(filter_from_u32(9).is_err());
    assert_eq!(scaler_from_u32(9), Scaler::Auto);
}

#[test]
fn socket_names_are_read_and_sorted() {
    let dir = std::env::temp_dir().join(format!("kotori-x11-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["X2", "X10", "X0", "not-a-display", "Xabc", "X1"] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    assert_eq!(display_numbers(&dir), vec![0, 1, 2, 10]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_socket_directory_is_not_an_error() {
    assert!(display_numbers(Path::new("/nonexistent/x11")).is_empty());
}

#[test]
fn an_empty_socket_directory_finds_nothing() {
    let dir = std::env::temp_dir().join(format!("kotori-x11-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(GamescopeDisplay::discover_in(&dir, 1234).unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

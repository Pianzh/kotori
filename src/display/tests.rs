//! `display` 的测试:超时击杀与输出回收、kscreen-doctor 的 JSON 解析。
//! 从 `mod.rs` 整体搬来(纯移动)—— mod.rs 曾卡在 500 行硬线上。

use super::*;

// 这两条用 `/bin/sh`（POSIX 保证存在的绝对路径），不是 PATH 上的外部程序：
// CI 规则禁的是 rclone / secret-tool 这类"这台机器上可能没装"的工具。
#[cfg(unix)]
#[test]
fn a_probe_that_never_answers_is_killed_and_read_as_no_answer() {
    let started = Instant::now();
    let out = probe_output_within("/bin/sh", &["-c", "sleep 60"], Duration::from_millis(200));
    assert!(out.is_none(), "卡住的探针必须当成「没有答案」");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "必须在超时后放弃，而不是等它自己结束（实耗 {:?}）",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn a_probe_that_answers_hands_back_its_output() {
    let out = probe_output_within("/bin/sh", &["-c", "printf hi"], Duration::from_secs(5))
        .expect("应当拿到输出");
    assert!(out.status.success());
    assert_eq!(out.stdout, b"hi");
}

const FOCUSED_OUTPUT: &str = r#"{
        "name": "DP-3",
        "modes": [
            {"width": 2560, "height": 1440, "refresh_rate": 60000, "is_preferred": true},
            {"width": 2560, "height": 1440, "refresh_rate": 144000, "is_preferred": false},
            {"width": 1920, "height": 1080, "refresh_rate": 60000, "is_preferred": false}
        ],
        "current_mode": 1,
        "logical": {"x": 0, "y": 0, "width": 2048, "height": 1152, "scale": 1.25}
    }"#;

const ALL_OUTPUTS: &str = r#"{
        "eDP-1": {
            "name": "eDP-1",
            "modes": [
                {"width": 2560, "height": 1600, "refresh_rate": 60000},
                {"width": 2560, "height": 1600, "refresh_rate": 165000}
            ],
            "current_mode": 1
        },
        "DP-3": {
            "name": "DP-3",
            "modes": [
                {"width": 2560, "height": 1440, "refresh_rate": 144000},
                {"width": 1920, "height": 1080, "refresh_rate": 60000}
            ],
            "current_mode": 0
        }
    }"#;

#[test]
fn uses_current_mode_not_logical_size() {
    let value: Value = serde_json::from_str(FOCUSED_OUTPUT).unwrap();
    // logical is 2048x1152 (scale 1.25); the physical mode is 2560x1440.
    assert_eq!(resolution_from_output_json(&value), Some((2560, 1440)));
}

#[test]
fn picks_largest_output_deterministically() {
    let value: Value = serde_json::from_str(ALL_OUTPUTS).unwrap();
    // eDP-1 2560x1600 beats DP-3 2560x1440.
    assert_eq!(
        largest_resolution_from_outputs_json(&value),
        Some((2560, 1600))
    );
}

#[test]
fn output_without_modes_is_skipped() {
    let value: Value = serde_json::from_str(r#"{"HDMI-A-1":{"name":"HDMI-A-1"}}"#).unwrap();
    assert_eq!(largest_resolution_from_outputs_json(&value), None);
}

#[test]
fn out_of_range_current_mode_falls_back_to_first() {
    let value: Value =
        serde_json::from_str(r#"{"modes":[{"width":1280,"height":720}],"current_mode":9}"#)
            .unwrap();
    assert_eq!(resolution_from_output_json(&value), Some((1280, 720)));
}

#[test]
fn parses_resolution_specs() {
    assert_eq!(parse_resolution_spec("2560x1440"), Some((2560, 1440)));
    assert_eq!(parse_resolution_spec(" 1920X1080 "), Some((1920, 1080)));
    assert_eq!(parse_resolution_spec("2560"), None);
    assert_eq!(parse_resolution_spec("0x1440"), None);
    assert_eq!(parse_resolution_spec("axb"), None);
}

/// Shape captured from a real `kscreen-doctor -j` on Plasma 6 / Wayland:
/// `scale` is 1.5 and `screen.currentSize` is the *logical* 1707x1067.
const KSCREEN_LAPTOP: &str = r#"{
        "features": 255,
        "outputs": [
            {
                "brightness": 0.85,
                "connected": true,
                "currentModeId": "2",
                "enabled": true,
                "id": 1,
                "modes": [
                    {"id": "1", "name": "2560x1600@60", "refreshRate": 60,
                     "size": {"height": 1600, "width": 2560}},
                    {"id": "2", "name": "2560x1600@165", "refreshRate": 165,
                     "size": {"height": 1600, "width": 2560}},
                    {"id": "3", "name": "1280x800@60", "refreshRate": 60,
                     "size": {"height": 800, "width": 1280}}
                ],
                "name": "eDP-1",
                "pos": {"x": 0, "y": 0},
                "priority": 0,
                "rotation": 1,
                "scale": 1.5,
                "size": {"height": 1600, "width": 2560},
                "type": 7
            }
        ],
        "screen": {"currentSize": {"height": 1067, "width": 1707}}
    }"#;

#[test]
fn kde_uses_device_pixels_not_logical_size() {
    let value: Value = serde_json::from_str(KSCREEN_LAPTOP).unwrap();
    // logical screen size is 1707x1067 (scale 1.5) and must be ignored.
    assert_eq!(resolution_from_kscreen_json(&value), Some((2560, 1600)));
}

#[test]
fn kde_follows_current_mode_id() {
    let value: Value = serde_json::from_str(
        r#"{"outputs":[{"connected":true,"enabled":true,"currentModeId":"2","name":"eDP-1",
                "size":{"width":1280,"height":800},
                "modes":[{"id":"1","size":{"width":2560,"height":1600}},
                         {"id":"2","size":{"width":1280,"height":800}}]}]}"#,
    )
    .unwrap();
    // The active mode is the 1280x800 one, not the first (or the largest).
    assert_eq!(resolution_from_kscreen_json(&value), Some((1280, 800)));
}

#[test]
fn kde_prefers_primary_and_skips_unusable_outputs() {
    let value: Value = serde_json::from_str(
        r#"{"outputs":[
                {"connected":true,"enabled":true,"currentModeId":"1","name":"DP-3","priority":0,
                 "modes":[{"id":"1","size":{"width":2560,"height":1440}}]},
                {"connected":true,"enabled":true,"currentModeId":"1","name":"HDMI-A-1","priority":1,
                 "modes":[{"id":"1","size":{"width":1920,"height":1080}}]},
                {"connected":true,"enabled":false,"currentModeId":"1","name":"DP-1","priority":9,
                 "modes":[{"id":"1","size":{"width":3840,"height":2160}}]},
                {"connected":false,"enabled":true,"currentModeId":"1","name":"DP-2","priority":9,
                 "modes":[{"id":"1","size":{"width":3840,"height":2160}}]}
            ]}"#,
    )
    .unwrap();
    // The disabled 4K and the unplugged 4K must not win, and the primary
    // (priority 1) beats the bigger DP-3.
    assert_eq!(resolution_from_kscreen_json(&value), Some((1920, 1080)));
}

#[test]
fn kde_without_priority_falls_back_to_largest() {
    let value: Value = serde_json::from_str(
        r#"{"outputs":[
                {"connected":true,"enabled":true,"currentModeId":"1","name":"DP-3","priority":0,
                 "modes":[{"id":"1","size":{"width":2560,"height":1440}}]},
                {"connected":true,"enabled":true,"currentModeId":"1","name":"eDP-1","priority":0,
                 "modes":[{"id":"1","size":{"width":2560,"height":1600}}]}
            ]}"#,
    )
    .unwrap();
    // KScreen leaves priority at 0 on single-output setups and when the user
    // never picked a primary, so fall back to the same rule as Niri.
    assert_eq!(resolution_from_kscreen_json(&value), Some((2560, 1600)));
}

#[test]
fn kde_unknown_mode_id_falls_back_to_output_size() {
    let value: Value = serde_json::from_str(
        r#"{"outputs":[{"connected":true,"enabled":true,"currentModeId":"99","name":"eDP-1",
                "size":{"width":2560,"height":1600},
                "modes":[{"id":"1","size":{"width":1280,"height":720}}]}]}"#,
    )
    .unwrap();
    assert_eq!(resolution_from_kscreen_json(&value), Some((2560, 1600)));
}

#[test]
fn kde_numeric_mode_ids_and_missing_keys_are_tolerated() {
    // Older KScreen serialised ids as numbers, and a payload with no
    // `connected`/`enabled` keys should be treated as usable.
    let value: Value = serde_json::from_str(
        r#"{"outputs":[{"currentModeId":2,"name":"eDP-1",
                "modes":[{"id":1,"size":{"width":3840,"height":2160}},
                         {"id":2,"size":{"width":2560,"height":1600}}]}]}"#,
    )
    .unwrap();
    assert_eq!(resolution_from_kscreen_json(&value), Some((2560, 1600)));
}

#[test]
fn kde_without_outputs_returns_none() {
    assert_eq!(resolution_from_kscreen_json(&serde_json::json!({})), None);
    assert_eq!(
        resolution_from_kscreen_json(&serde_json::json!({"outputs": []})),
        None
    );
    let all_unplugged: Value = serde_json::from_str(
        r#"{"outputs":[{"connected":false,"enabled":false,"currentModeId":"1","name":"DP-1",
                "modes":[{"id":"1","size":{"width":3840,"height":2160}}]}]}"#,
    )
    .unwrap();
    assert_eq!(resolution_from_kscreen_json(&all_unplugged), None);
}

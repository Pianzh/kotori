//! Display / output resolution discovery.
//!
//! On Niri the compositor tiles every window to a whole output, so the
//! gamescope *output* size should match the physical resolution of the monitor
//! the game lands on (ADR-004). Hard-coding a resolution was wrong: it silently
//! produced a wrong scaling ratio on any other machine.
//!
//! Resolution order:
//!   1. `KOTORI_OUTPUT_RESOLUTION=WxH` (explicit escape hatch / tests)
//!   2. Niri focused output
//!   3. Largest connected Niri output
//!   4. `None` — callers decide on their own fallback.

use serde::Deserialize;
use serde_json::Value;

/// Environment variable overriding the detected output resolution (`WxH`).
pub const OUTPUT_RESOLUTION_ENV: &str = "KOTORI_OUTPUT_RESOLUTION";

#[derive(Debug, Deserialize)]
struct OutputMode {
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
struct OutputInfo {
    modes: Vec<OutputMode>,
    current_mode: usize,
}

/// Physical resolution of the primary output, or `None` if it cannot be determined.
pub fn primary_resolution() -> Option<(u32, u32)> {
    if let Ok(spec) = std::env::var(OUTPUT_RESOLUTION_ENV) {
        match parse_resolution_spec(&spec) {
            Some(res) => return Some(res),
            None => tracing::warn!("{OUTPUT_RESOLUTION_ENV}={spec:?} 不是 WxH 格式，已忽略"),
        }
    }

    if let Some(res) = niri_json(&["focused-output"]).and_then(|v| resolution_from_output_json(&v))
    {
        tracing::debug!("focused output resolution: {}x{}", res.0, res.1);
        return Some(res);
    }

    if let Some(res) =
        niri_json(&["outputs"]).and_then(|v| largest_resolution_from_outputs_json(&v))
    {
        tracing::debug!("largest output resolution: {}x{}", res.0, res.1);
        return Some(res);
    }

    tracing::debug!("无法探测显示器分辨率，调用方将使用回退值");
    None
}

/// Primary resolution with a caller-supplied fallback.
pub fn primary_resolution_or(fallback: (u32, u32)) -> (u32, u32) {
    primary_resolution().unwrap_or(fallback)
}

fn niri_json(args: &[&str]) -> Option<Value> {
    // `niri msg` fails fast when no compositor is reachable, so no timeout is
    // needed here.
    let output = std::process::Command::new("niri")
        .arg("msg")
        .arg("--json")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        tracing::debug!("niri msg {:?} failed: {}", args, output.status);
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Parse `"2560x1440"` (also tolerates `2560X1440` and surrounding spaces).
fn parse_resolution_spec(spec: &str) -> Option<(u32, u32)> {
    let (w, h) = spec.trim().split_once(['x', 'X'])?;
    let w = w.trim().parse::<u32>().ok()?;
    let h = h.trim().parse::<u32>().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Physical resolution of one `niri msg --json <output>` object.
fn resolution_from_output_json(value: &Value) -> Option<(u32, u32)> {
    let info: OutputInfo = serde_json::from_value(value.clone()).ok()?;
    // `current_mode` indexes `modes`; `logical` is the *scaled* size and must
    // not be used for the gamescope output size.
    let mode = info
        .modes
        .get(info.current_mode)
        .or_else(|| info.modes.first())?;
    Some((mode.width, mode.height))
}

/// Largest current-mode area across all connected outputs (deterministic).
fn largest_resolution_from_outputs_json(value: &Value) -> Option<(u32, u32)> {
    let outputs = value.as_object()?;
    let mut best: Option<(String, (u32, u32))> = None;
    for (name, output) in outputs {
        let Some(res) = resolution_from_output_json(output) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((best_name, (bw, bh))) => {
                let area = res.0 as u64 * res.1 as u64;
                let best_area = *bw as u64 * *bh as u64;
                area > best_area || (area == best_area && name < best_name)
            }
        };
        if better {
            best = Some((name.clone(), res));
        }
    }
    best.map(|(_, res)| res)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

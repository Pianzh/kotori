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
//!   4. KDE primary output (`kscreen-doctor -j`)
//!   5. `None` — callers decide on their own fallback.
//!
//! Niri and KDE are tried in that order because each probe fails fast when its
//! compositor is absent: on KDE `niri msg` exits non-zero immediately, and on
//! niri `kscreen-doctor` reports no outputs.
//!
//! ⚠ **Every probe is bounded in time** ([`PROBE_TIMEOUT`]). "Fails fast when
//! its compositor is absent" is not the same as "always answers": these are
//! clients of a compositor, and a compositor that accepts the connection but
//! never answers leaves them running for good. That is not hypothetical — on
//! the ARM target's bridged-Wayland session (`anland` v2) `kscreen-doctor`
//! never exited, which hung `launch` forever and leaked one process per
//! attempt (2026-09-14). The caller of this module is the launch path, so a
//! probe without a timeout is a hang waiting to happen.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// Environment variable overriding the detected output resolution (`WxH`).
pub const OUTPUT_RESOLUTION_ENV: &str = "KOTORI_OUTPUT_RESOLUTION";

/// How long one external compositor probe may take before we kill it.
///
/// The honest probes answer in tens of milliseconds; this only has to be long
/// enough not to cut off a slow machine under load (an ARM container can be
/// very slow) and short enough not to stall a launch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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

    if let Some(res) = kscreen_doctor_json().and_then(|v| resolution_from_kscreen_json(&v)) {
        tracing::debug!("KDE primary output resolution: {}x{}", res.0, res.1);
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
    let mut argv = vec!["msg", "--json"];
    argv.extend_from_slice(args);
    let output = probe_output("niri", &argv)?;
    if !output.status.success() {
        tracing::debug!("niri msg {:?} failed: {}", args, output.status);
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Run one probe to completion, capturing its output, and kill it if it
/// overstays [`PROBE_TIMEOUT`].
///
/// `None` means "no answer" — not on this machine, exited non-zero, printed
/// garbage, or hung. Every caller treats those the same way (try the next
/// probe, then fall back), which is the point: none of them may block a launch.
fn probe_output(program: &str, args: &[&str]) -> Option<Output> {
    probe_output_within(program, args, PROBE_TIMEOUT)
}

/// [`probe_output`] with an explicit deadline (tests use a short one).
fn probe_output_within(program: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            // Exited on its own: `wait_with_output` reaps it and drains both
            // pipes (they are already closed, so this cannot block).
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {}
            Err(err) => {
                tracing::debug!("{program} 探测失败：{err}");
                return None;
            }
        }
        if Instant::now() >= deadline {
            // Killing is not a courtesy here: a probe that never answers would
            // otherwise sit in the process table forever, one per launch.
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("{program} 探测超过 {timeout:?} 没有返回，放弃（当作没有这个桌面）");
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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

/// KDE: `kscreen-doctor -j` lists every configured output.
fn kscreen_doctor_json() -> Option<Value> {
    let output = probe_output("kscreen-doctor", &["-j"])?;
    if !output.status.success() {
        tracing::debug!("kscreen-doctor -j failed: {}", output.status);
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// One entry of `kscreen-doctor -j`'s `outputs` array.
#[derive(Debug, Deserialize)]
struct KScreenOutput {
    #[serde(default = "default_true")]
    connected: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default, rename = "currentModeId")]
    current_mode_id: Option<Value>,
    #[serde(default)]
    modes: Vec<KScreenMode>,
    /// Higher means "primary"; KScreen leaves it at 0 on single-output setups.
    #[serde(default)]
    priority: i64,
    #[serde(default)]
    name: String,
    /// Current size in *device* pixels (`screen.currentSize` is the logical,
    /// scaled one and must not be used here).
    #[serde(default)]
    size: Option<KScreenSize>,
}

#[derive(Debug, Deserialize)]
struct KScreenMode {
    id: Value,
    size: KScreenSize,
}

#[derive(Debug, Deserialize)]
struct KScreenSize {
    width: u32,
    height: u32,
}

fn default_true() -> bool {
    true
}

/// Ids are strings in today's `kscreen-doctor`, numbers in older KScreen.
fn id_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

impl KScreenOutput {
    /// Pixel size of the mode currently driving this output.
    fn current_resolution(&self) -> Option<(u32, u32)> {
        let by_id = self.current_mode_id.as_ref().and_then(|id| {
            let wanted = id_as_string(id)?;
            self.modes
                .iter()
                .find(|m| id_as_string(&m.id).as_deref() == Some(wanted.as_str()))
                .map(|m| (m.size.width, m.size.height))
        });
        // If the id matches nothing (schema moved on), the output's own `size`
        // describes what is on screen right now, which beats guessing a mode.
        // `modes.first()` is the last resort for payloads with no size field.
        let size = by_id
            .or_else(|| self.size.as_ref().map(|s| (s.width, s.height)))
            .or_else(|| self.modes.first().map(|m| (m.size.width, m.size.height)))?;
        (size.0 > 0 && size.1 > 0).then_some(size)
    }
}

/// Pixel resolution of KDE's primary output, falling back to the largest
/// enabled one (deterministic by name, mirroring the Niri rule).
fn resolution_from_kscreen_json(value: &Value) -> Option<(u32, u32)> {
    let outputs: Vec<KScreenOutput> = serde_json::from_value(value.get("outputs")?.clone()).ok()?;
    let mut best: Option<(i64, u64, &str, (u32, u32))> = None;
    for output in &outputs {
        if !output.connected || !output.enabled {
            continue;
        }
        let Some(res) = output.current_resolution() else {
            continue;
        };
        let area = res.0 as u64 * res.1 as u64;
        let candidate = (output.priority, area, output.name.as_str(), res);
        let better = match &best {
            None => true,
            Some((priority, best_area, best_name, _)) => {
                candidate.0 > *priority
                    || (candidate.0 == *priority
                        && (area > *best_area
                            || (area == *best_area && output.name.as_str() < *best_name)))
            }
        };
        if better {
            best = Some(candidate);
        }
    }
    best.map(|(_, _, _, res)| res)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 这两条用 `/bin/sh`（POSIX 保证存在的绝对路径），不是 PATH 上的外部程序：
    // CI 规则禁的是 rclone / secret-tool 这类"这台机器上可能没装"的工具。
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
}

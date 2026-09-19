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
mod tests;

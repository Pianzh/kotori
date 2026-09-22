//! e2e 的共用小工具：断言、脚本、等待、以及"云上有什么"。
//!
//! 与 `fixture` 分开：那边是**一个 daemon 的生命周期**，这边是不依赖任何夹具的
//! 纯辅助函数。

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::fixture::Fixture;

pub(crate) fn assert_is_error(response: &Value, code: i32) {
    let value: Value = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(
        value["error"]["code"], code,
        "expected error {code}, got {response}"
    );
}

/// Write an executable helper script and prove the kernel will run it.
///
/// The tests run in parallel, so another thread may fork between our write and
/// our first exec and inherit the still-open write handle — the kernel then
/// reports `ETXTBSY` for that inode until the child execs. Retrying converges,
/// because once our own write handle is closed the file cannot be reopened for
/// writing. Every fake binary answers `--kotori-warmup` with an immediate exit,
/// so this has no side effects.
pub(crate) fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();

    for _ in 0..200 {
        match Command::new(path).arg("--kotori-warmup").output() {
            Ok(_) => return,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("cannot execute {}: {e}", path.display()),
        }
    }
    panic!("{} stayed busy", path.display());
}

/// Poll `check` until it returns true or the deadline passes.
pub(crate) fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    check()
}

/// The version packages the fake bucket holds for a game, oldest first.
///
/// "One version, one package" is the whole point of the layout, so the number of
/// `.zip` objects *is* the version count.
pub(crate) fn cloud_packages(game_dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(game_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".zip"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Every `rclone` invocation the daemon made.
pub(crate) fn rclone_calls(fixture: &Fixture) -> Vec<String> {
    std::fs::read_to_string(fixture.dir.join("rclone.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("argv:"))
        .map(str::to_string)
        .collect()
}

/// 从 `config.toml` 里抠出一个键的值（测试只关心"写没写、写成了什么"）。
pub(crate) fn field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&format!("{key} = ")))
        .map(|value| value.trim_matches('"').to_string())
}

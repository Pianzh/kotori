//! Crash reporting: a panic hook that leaves the report on disk.

use super::*;

/// Crash report file, next to the daemon log.
const UI_CRASH_LOG: &str = "ui-crash.log";

/// Record a panic before the process dies.
///
/// The GUI lives in a terminal the user closes as soon as something goes
/// wrong, and stderr dies with it — which is how a crash becomes "it just
/// exited for no reason". Keeping the report on disk makes it explainable.
///
/// Returns the path and whether it could actually be written to; the message
/// printed on a crash must not promise a file that is not there.
pub(super) fn install_crash_log() -> (PathBuf, bool) {
    let dir = crate::config::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(UI_CRASH_LOG);

    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    };
    // Find out now, while there is still someone to tell.
    let writable = open().is_ok();

    let report_path = path.clone();
    let note = if writable {
        format!("kotori 崩溃了，原因已写入 {}", path.display())
    } else {
        format!(
            "kotori 崩溃了：{} 写不进去（目录只读？），报告只在上面这段输出里",
            path.display()
        )
    };
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;

        let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let report = format!(
            "\n===== {when} =====\n{info}\n\nbacktrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&report_path)
        {
            let _ = file.write_all(report.as_bytes());
        }
        eprint!("{report}");
        eprintln!("{note}");
    }));

    (path, writable)
}

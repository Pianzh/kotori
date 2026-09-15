//! Finding the external programs kotori drives.
//!
//! Everything here is a **lookup**, not a run: callers spawn the program
//! themselves (each needs its own stdin/stdout/env handling), and the
//! `KOTORI_*` overrides in `config::paths` take precedence over `PATH`, so
//! tests never depend on what the machine happens to have installed.

use std::path::PathBuf;

/// Find a binary in PATH.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

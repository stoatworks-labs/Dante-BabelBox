//! Where logs and crash reports live.
//!
//! Platform convention rather than a dot-directory next to the binary: on a
//! show machine the binary often sits on a read-only image or a share, and a
//! log that cannot be written is worse than no log at all because nobody
//! finds out until they need it.

use std::path::PathBuf;

/// Directory for this app's logs, crash reports and bundles.
///
/// `{PREFIX}_LOG_DIR` overrides it outright, which is how you point a whole
/// rack at one collected location.
pub fn log_dir(app: &str, env_prefix: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(format!("{env_prefix}_LOG_DIR")) {
        return PathBuf::from(dir);
    }
    platform_log_dir(app)
}

#[cfg(target_os = "macos")]
fn platform_log_dir(app: &str) -> PathBuf {
    home().join("Library").join("Logs").join(app)
}

#[cfg(target_os = "windows")]
fn platform_log_dir(app: &str) -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("AppData").join("Local"));
    base.join(app).join("logs")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_log_dir(app: &str) -> PathBuf {
    // XDG puts logs under state, not cache: cache may be wiped at any time,
    // and the whole point of a crash report is that it outlives the crash.
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("state"));
    base.join(app).join("logs")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

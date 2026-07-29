//! The single-file diagnostics bundle.
//!
//! One file, so that "send me your diagnostics" is one instruction and not a
//! conversation about which of six files were wanted. JSON, so a tool can
//! parse it; logs held as arrays of lines, so a person can still read it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::report::{AppInfo, PlatformInfo, ProcessInfo, SCHEMA};

/// How many rotated log files to include, newest first.
const MAX_LOG_FILES: usize = 3;
/// Per-file line cap, so one runaway log cannot make the bundle unusable.
const MAX_LINES_PER_FILE: usize = 5_000;
/// How many crash reports to carry, newest first.
const MAX_CRASH_REPORTS: usize = 5;

#[derive(Debug, Serialize)]
pub struct Bundle {
    pub schema: &'static str,
    pub kind: &'static str,
    pub generated_at: String,
    pub app: AppInfo,
    pub platform: PlatformInfo,
    pub process: ProcessInfo,
    pub config: serde_json::Value,
    pub log_dir: String,
    /// Crash reports found alongside the logs, newest first. Embedded whole,
    /// so the bundle stays self-contained.
    pub crash_reports: Vec<serde_json::Value>,
    pub logs: Vec<LogFile>,
    /// Lines still in memory. On a bundle taken from a running process these
    /// are fresher than the log file, which is written non-blocking.
    pub recent_log: Vec<String>,
    /// Anything that went wrong while assembling the bundle. Collection is
    /// best-effort — a missing log file must not stop the rest being sent.
    pub collection_warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LogFile {
    pub file: String,
    pub bytes: u64,
    pub truncated: bool,
    pub lines: Vec<String>,
}

/// Assemble a bundle from the log directory and write it there.
///
/// Returns the path to hand to the user.
pub fn collect(dir: &Path) -> std::io::Result<PathBuf> {
    let installed = crate::installed()
        .ok_or_else(|| std::io::Error::other("diag::init has not been called"))?;

    let mut warnings = Vec::new();
    let entries = read_dir_sorted(dir, &mut warnings);

    let crash_reports = entries
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains("-crash-") && n.ends_with(".json"))
        })
        .take(MAX_CRASH_REPORTS)
        .filter_map(|p| match std::fs::read_to_string(p) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(v) => Some(v),
                Err(e) => {
                    warnings.push(format!("{}: not valid JSON: {e}", p.display()));
                    None
                }
            },
            Err(e) => {
                warnings.push(format!("{}: {e}", p.display()));
                None
            }
        })
        .collect();

    let logs = entries
        .iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("log")
                || p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".log"))
        })
        .take(MAX_LOG_FILES)
        .filter_map(|p| read_log(p, &mut warnings))
        .collect();

    let bundle = Bundle {
        schema: SCHEMA,
        kind: "diagnostics-bundle",
        generated_at: crate::stamp_rfc3339(),
        app: installed.app_info(),
        platform: PlatformInfo::capture(installed.hostname.clone()),
        process: installed.process_info(),
        config: installed.config_snapshot(),
        log_dir: dir.display().to_string(),
        crash_reports,
        logs,
        recent_log: installed.ring.snapshot(),
        collection_warnings: warnings,
    };

    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!(
        "{}-diagnostics-{}.json",
        installed.app,
        crate::stamp_compact()
    ));
    let json = serde_json::to_vec_pretty(&bundle)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Directory contents, newest first.
fn read_dir_sorted(dir: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let iter = match std::fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(e) => {
            warnings.push(format!("could not read {}: {e}", dir.display()));
            return Vec::new();
        }
    };

    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = iter
        .filter_map(Result::ok)
        .filter_map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, e.path()))
        })
        .collect();
    entries.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    entries.into_iter().map(|(_, p)| p).collect()
}

fn read_log(path: &Path, warnings: &mut Vec<String>) -> Option<LogFile> {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return None;
        }
    };

    let all: Vec<&str> = text.lines().collect();
    let truncated = all.len() > MAX_LINES_PER_FILE;
    // Keep the tail, not the head: whatever went wrong happened at the end.
    let lines = all[all.len().saturating_sub(MAX_LINES_PER_FILE)..]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    Some(LogFile {
        file: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_owned(),
        bytes,
        truncated,
        lines,
    })
}

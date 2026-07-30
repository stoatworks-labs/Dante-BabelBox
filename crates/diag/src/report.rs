//! The machine-readable crash report.
//!
//! Written on panic, before the process finishes unwinding. The shape is
//! stable and documented in `docs/diagnostics.md`; treat `SCHEMA` as the
//! contract and bump it if a field changes meaning.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Identifies the document shape to anything reading it later.
pub const SCHEMA: &str = "stoatworks.diagnostics/1";

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    /// Short git revision, suffixed `-dirty` if the tree had uncommitted
    /// changes when the binary was built.
    pub git_rev: String,
    pub built_at: String,
}

#[derive(Debug, Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub family: String,
    pub hostname: String,
    pub available_parallelism: Option<usize>,
}

impl PlatformInfo {
    pub fn capture(hostname: String) -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            family: std::env::consts::FAMILY.to_owned(),
            hostname,
            available_parallelism: std::thread::available_parallelism().ok().map(|n| n.get()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub args: Vec<String>,
    pub started_at: String,
}

#[derive(Debug, Serialize)]
pub struct PanicDetail {
    pub message: String,
    pub location: Option<String>,
    pub thread: String,
    pub backtrace: String,
}

#[derive(Debug, Serialize)]
pub struct CrashReport {
    pub schema: &'static str,
    pub kind: &'static str,
    pub generated_at: String,
    pub app: AppInfo,
    pub platform: PlatformInfo,
    pub process: ProcessInfo,
    /// The effective configuration, with anything secret-looking replaced.
    pub config: serde_json::Value,
    pub panic: PanicDetail,
    /// The last few hundred log lines, oldest first.
    pub recent_log: Vec<String>,
}

impl CrashReport {
    /// Write the report next to the logs.
    ///
    /// Falls back to the temp directory: a crash report that cannot be written
    /// because the log directory vanished is the one case where being noisy
    /// and writing somewhere unexpected beats being tidy and writing nowhere.
    pub fn write(&self, dir: &Path) -> std::io::Result<PathBuf> {
        let name = format!("{}-crash-{}.json", self.app.name, crate::stamp_compact());
        match write_to(dir, &name, self) {
            Ok(path) => Ok(path),
            Err(_) => write_to(&std::env::temp_dir(), &name, self),
        }
    }
}

fn write_to<T: Serialize>(dir: &Path, name: &str, value: &T) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Redact values whose key looks like a secret.
///
/// Deliberately over-eager: a redacted port number costs nothing, a leaked
/// token in a file the user emails to a mailing list costs a great deal.
pub fn redact(value: &serde_json::Value) -> serde_json::Value {
    const SENSITIVE: &[&str] = &[
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "apikey",
        "api_key",
        "credential",
        "auth",
        "private",
    ];

    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let flat = k.to_ascii_lowercase().replace(['-', '_'], "");
                let sensitive = SENSITIVE.iter().any(|s| flat.contains(&s.replace('_', "")));
                out.insert(
                    k.clone(),
                    if sensitive {
                        serde_json::Value::String("<redacted>".into())
                    } else {
                        redact(v)
                    },
                );
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_secret_looking_keys_at_any_depth() {
        let input = json!({
            "host": "10.0.0.5",
            "api_key": "sk-live-1234",
            "nested": { "authToken": "abcd", "port": 5250 },
            "list": [{ "password": "hunter2" }],
        });

        let out = redact(&input);

        assert_eq!(out["host"], json!("10.0.0.5"));
        assert_eq!(out["api_key"], json!("<redacted>"));
        assert_eq!(out["nested"]["authToken"], json!("<redacted>"));
        assert_eq!(out["nested"]["port"], json!(5250));
        assert_eq!(out["list"][0]["password"], json!("<redacted>"));
    }
}

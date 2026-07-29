//! Logging and crash diagnostics.
//!
//! Three things, because a show machine fails in three different ways and
//! each needs a different artefact:
//!
//! 1. A **rotating human-readable log**, so an operator can see what happened.
//! 2. A **machine-readable crash report** written on panic, carrying the build
//!    identity, the platform, the redacted config, the last few hundred log
//!    lines and a backtrace — enough to diagnose a fault without a repro.
//! 3. A **single-file diagnostics bundle** on demand, so "send me your
//!    diagnostics" is one instruction.
//!
//! ```no_run
//! # #[derive(serde::Serialize)]
//! # struct Args { host: String, port: u16 }
//! # let args = Args { host: "127.0.0.1".into(), port: 5250 };
//! let _guard = diag::init(
//!     diag::Options::new("caspar-avd", "CASPAR_AV", env!("CARGO_PKG_VERSION"))
//!         .with_default_filter("info,tower_http=warn")
//!         .with_config(&args),
//! )?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Hold the returned guard for the life of the process: dropping it flushes
//! the file writer. Dropping it early — `let _ = diag::init(..)` — silently
//! stops the log file being written.

pub mod bundle;
pub mod paths;
pub mod report;
mod ring;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use time::OffsetDateTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use report::{AppInfo, CrashReport, PanicDetail, PlatformInfo, ProcessInfo, SCHEMA};
use ring::{Ring, RingLayer};

/// How many days of rotated logs to keep.
const KEEP_LOG_FILES: usize = 7;

/// What to call things and where to put them.
pub struct Options {
    app: String,
    env_prefix: String,
    version: String,
    default_filter: String,
    config: serde_json::Value,
}

impl Options {
    /// `app` names the log files, `env_prefix` scopes the environment
    /// variables (`{PREFIX}_LOG`, `{PREFIX}_LOG_DIR`), `version` should be the
    /// calling crate's `env!("CARGO_PKG_VERSION")`.
    pub fn new(app: &str, env_prefix: &str, version: &str) -> Self {
        Self {
            app: app.to_owned(),
            env_prefix: env_prefix.to_owned(),
            version: version.to_owned(),
            default_filter: "info".to_owned(),
            config: serde_json::Value::Null,
        }
    }

    /// Filter used when neither `{PREFIX}_LOG` nor `RUST_LOG` is set.
    pub fn with_default_filter(mut self, filter: &str) -> Self {
        self.default_filter = filter.to_owned();
        self
    }

    /// Snapshot the effective configuration into crash reports and bundles.
    ///
    /// Secret-looking keys are redacted here, once, so no caller has to
    /// remember to do it.
    pub fn with_config<T: Serialize>(mut self, config: &T) -> Self {
        self.config = match serde_json::to_value(config) {
            Ok(value) => report::redact(&value),
            Err(e) => serde_json::Value::String(format!("<config not serializable: {e}>")),
        };
        self
    }
}

/// Process-wide state the panic hook and the bundler both need.
struct Installed {
    app: String,
    version: String,
    dir: PathBuf,
    /// Behind a lock because it can be attached after `init` — see
    /// `set_config`, which exists so logging can be up before the config file
    /// is read.
    config: std::sync::RwLock<serde_json::Value>,
    ring: Arc<Ring>,
    hostname: String,
    args: Vec<String>,
    started_at: String,
}

impl Installed {
    /// Read the stored config, recovering from a poisoned lock rather than
    /// panicking: this is read from the panic hook.
    fn config_snapshot(&self) -> serde_json::Value {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn app_info(&self) -> AppInfo {
        AppInfo {
            name: self.app.clone(),
            version: self.version.clone(),
            git_rev: env!("DIAG_GIT_REV").to_owned(),
            built_at: built_at(),
        }
    }

    fn process_info(&self) -> ProcessInfo {
        ProcessInfo {
            pid: std::process::id(),
            args: self.args.clone(),
            started_at: self.started_at.clone(),
        }
    }
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

fn installed() -> Option<&'static Installed> {
    INSTALLED.get()
}

/// Flushes the log file on drop. Keep it alive for the whole process.
#[must_use = "dropping the guard stops the log file being written"]
pub struct Guard {
    _worker: tracing_appender::non_blocking::WorkerGuard,
}

/// Install logging and the crash handler.
///
/// Safe to call once; a second call returns an error rather than quietly
/// installing a second subscriber.
pub fn init(options: Options) -> std::io::Result<Guard> {
    let dir = paths::log_dir(&options.app, &options.env_prefix);
    std::fs::create_dir_all(&dir)?;

    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(&options.app)
        .filename_suffix("log")
        .max_log_files(KEEP_LOG_FILES)
        .build(&dir)
        .map_err(std::io::Error::other)?;
    let (file_writer, worker) = tracing_appender::non_blocking(appender);

    let filter_spec = std::env::var(format!("{}_LOG", options.env_prefix))
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| options.default_filter.clone());
    let filter = EnvFilter::try_new(&filter_spec).map_err(std::io::Error::other)?;

    let ring = Arc::new(Ring::new(ring::CAPACITY));

    tracing_subscriber::registry()
        .with(filter)
        // Console logs go to stderr, never stdout. Otherwise any command
        // whose output is meant to be consumed — `--collect-diagnostics`
        // prints a path — has its stdout corrupted by log lines.
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        // No ANSI in the file: colour escapes in a log someone opens in
        // Notepad turn it into noise.
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .with(RingLayer::new(ring.clone()))
        .try_init()
        .map_err(std::io::Error::other)?;

    let state = Installed {
        app: options.app.clone(),
        version: options.version.clone(),
        dir: dir.clone(),
        config: std::sync::RwLock::new(options.config),
        ring,
        hostname: hostname(),
        args: std::env::args().collect(),
        started_at: stamp_rfc3339(),
    };
    INSTALLED
        .set(state)
        .map_err(|_| std::io::Error::other("diag::init called twice"))?;

    install_panic_hook();

    let info = installed().expect("just set");
    tracing::info!(
        version = %info.version,
        git_rev = env!("DIAG_GIT_REV"),
        log_dir = %dir.display(),
        filter = %filter_spec,
        "logging started"
    );

    Ok(Guard { _worker: worker })
}

/// Attach or replace the effective configuration.
///
/// `Options::with_config` covers the common case. This exists for programs
/// whose configuration is read *after* logging starts — which is the right
/// order, because a fault while parsing a config file needs somewhere to be
/// recorded. Secret-looking keys are redacted here, as they are there.
pub fn set_config<T: Serialize>(config: &T) {
    let Some(installed) = installed() else { return };
    let value = match serde_json::to_value(config) {
        Ok(value) => report::redact(&value),
        Err(e) => serde_json::Value::String(format!("<config not serializable: {e}>")),
    };
    *installed
        .config
        .write()
        .unwrap_or_else(|e| e.into_inner()) = value;
}

/// Write a diagnostics bundle and return its path.
pub fn collect_diagnostics() -> std::io::Result<PathBuf> {
    let installed =
        installed().ok_or_else(|| std::io::Error::other("diag::init has not been called"))?;
    bundle::collect(&installed.dir)
}

/// The directory logs, crash reports and bundles are written to.
pub fn log_directory() -> Option<PathBuf> {
    installed().map(|i| i.dir.clone())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort throughout: a failure in here must not stop the normal
        // panic path from running.
        if let Some(installed) = installed() {
            let detail = PanicDetail {
                message: panic_message(info),
                location: info.location().map(|l| l.to_string()),
                thread: std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_owned(),
                backtrace: std::backtrace::Backtrace::force_capture().to_string(),
            };
            let report = CrashReport {
                schema: SCHEMA,
                kind: "crash-report",
                generated_at: stamp_rfc3339(),
                app: installed.app_info(),
                platform: PlatformInfo::capture(installed.hostname.clone()),
                process: installed.process_info(),
                config: installed.config_snapshot(),
                panic: detail,
                recent_log: installed.ring.snapshot(),
            };
            match report.write(&installed.dir) {
                Ok(path) => eprintln!(
                    "\n{} crashed. A diagnostic report was written to:\n  {}\nSend that file with your bug report — it contains the build, the \
                     configuration (secrets removed), the last log lines and a backtrace.\n",
                    installed.app,
                    path.display()
                ),
                Err(e) => eprintln!("\n{} crashed, and the diagnostic report could not be written: {e}\n", installed.app),
            }
        }
        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

fn hostname() -> String {
    if let Some(name) = std::env::var_os("COMPUTERNAME") {
        return name.to_string_lossy().into_owned();
    }
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned())
}

fn built_at() -> String {
    env!("DIAG_BUILT_AT_UNIX")
        .parse::<i64>()
        .ok()
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        .map(format_rfc3339)
        .unwrap_or_else(|| "unknown".to_owned())
}

/// `2026-07-29T14:15:00Z` — sortable, unambiguous, and what every log parser
/// already understands.
pub(crate) fn stamp_rfc3339() -> String {
    format_rfc3339(OffsetDateTime::now_utc())
}

fn format_rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// `20260729T141500Z` — the same instant, safe in a filename on Windows,
/// where `:` is not.
pub(crate) fn stamp_compact() -> String {
    let t = OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

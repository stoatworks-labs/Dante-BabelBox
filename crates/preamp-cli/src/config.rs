use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use dante_babelbox_core::{ChannelMapping, DeviceConfig};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default, rename = "device")]
    pub devices: Vec<DeviceConfig>,
    #[serde(default, rename = "mapping")]
    pub mappings: Vec<ChannelMapping>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for d in &self.devices {
            if !ids.insert(d.id.as_str()) {
                bail!("duplicate device id '{}' in config", d.id);
            }
            if !d.is_virtual && d.address.is_none() {
                bail!("device '{}' is not virtual but has no address", d.id);
            }
        }
        for m in &self.mappings {
            for addr in [&m.from, &m.to] {
                if !ids.contains(addr.device_id.as_str()) {
                    bail!("mapping references unknown device id '{}'", addr.device_id);
                }
            }
        }
        Ok(())
    }
}

/// Watches `path` and re-parses on every filesystem change, dropping bad
/// edits with a warning rather than tearing down the running bridge - a
/// config file left mid-edit shouldn't kill an otherwise-working daemon.
pub fn watch(path: PathBuf) -> Result<tokio::sync::watch::Receiver<Config>> {
    let initial = Config::load(&path)?;
    let (tx, rx) = tokio::sync::watch::channel(initial);

    std::thread::spawn(move || {
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};

        let (notify_tx, notify_rx) = std::sync::mpsc::channel();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(notify_tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "failed to start config file watcher");
                return;
            }
        };
        if let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive) {
            tracing::error!(error = %e, path = %path.display(), "failed to watch config file");
            return;
        }

        for res in notify_rx {
            let event = match res {
                Ok(event) => event,
                Err(e) => {
                    tracing::warn!(error = %e, "config watcher error");
                    continue;
                }
            };
            if !event.kind.is_modify() && !event.kind.is_create() {
                continue;
            }
            match Config::load(&path) {
                Ok(cfg) => {
                    tracing::info!(path = %path.display(), "reloaded config");
                    if tx.send(cfg).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to reload config, keeping previous version");
                }
            }
        }
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[[device]]
id = "sq-foh"
kind = "ah-midi"
address = "10.0.0.10"

[[device]]
id = "x32-monitors"
kind = "osc-x32"
address = "10.0.0.20"
port = 10023

[[mapping]]
from = { device = "sq-foh", channel = 3 }
to   = { device = "x32-monitors", channel = 7 }
bidirectional = true
"#;

    fn write_temp(contents: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bridge-test-{}-{n}.toml",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parses_and_validates_example_config() {
        let path = write_temp(EXAMPLE);
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.devices.len(), 2);
        assert_eq!(cfg.mappings.len(), 1);
        assert_eq!(cfg.devices[1].kind, "osc-x32");
        assert_eq!(cfg.devices[1].port, Some(10023));
        assert!(cfg.mappings[0].bidirectional);
    }

    #[test]
    fn rejects_mapping_to_unknown_device() {
        let bad = r#"
[[device]]
id = "sq-foh"
kind = "ah-midi"
address = "10.0.0.10"

[[mapping]]
from = { device = "sq-foh", channel = 3 }
to   = { device = "does-not-exist", channel = 7 }
"#;
        let path = write_temp(bad);
        let result = Config::load(&path);
        std::fs::remove_file(&path).ok();

        assert!(result.is_err());
    }

    #[test]
    fn rejects_non_virtual_device_with_no_address() {
        let bad = r#"
[[device]]
id = "sq-foh"
kind = "ah-midi"
"#;
        let path = write_temp(bad);
        let result = Config::load(&path);
        std::fs::remove_file(&path).ok();

        assert!(result.is_err());
    }

    #[test]
    fn accepts_virtual_device_with_no_address() {
        let ok = r#"
[[device]]
id = "future-x32"
kind = "osc-x32"
virtual = true
channels = 8
"#;
        let path = write_temp(ok);
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.devices.len(), 1);
        assert!(cfg.devices[0].is_virtual);
        assert_eq!(cfg.devices[0].address, None);
        assert_eq!(cfg.devices[0].channels, Some(8));
    }

    #[tokio::test]
    async fn watch_picks_up_a_real_file_edit() {
        let path = write_temp(EXAMPLE);
        let mut rx = watch(path.clone()).expect("failed to start watcher");

        // Initial value is available immediately without waiting on changed().
        assert_eq!(rx.borrow().mappings.len(), 1);

        let updated = EXAMPLE.replace("channel = 7", "channel = 9");

        // Retry the write rather than relying on a fixed pre-sleep to
        // guess when the OS-level watch is registered - a fixed guess
        // can still race under heavy parallel test load (seen flaking
        // under `cargo test --workspace`); writing the same content
        // repeatedly until the watcher notices converges regardless of
        // scheduling variance, since `watch::Sender::send` always marks
        // receivers changed, even for content-identical values.
        let mut noticed = false;
        for _ in 0..20 {
            std::fs::write(&path, &updated).unwrap();
            if tokio::time::timeout(std::time::Duration::from_millis(250), rx.changed()).await.is_ok() {
                noticed = true;
                break;
            }
        }
        assert!(noticed, "watch() never noticed the file edit after repeated writes");

        assert_eq!(rx.borrow().mappings[0].to.channel, 9);
        std::fs::remove_file(&path).ok();
    }
}

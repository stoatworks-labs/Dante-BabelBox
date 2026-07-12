use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use preamp_bridge_core::Mapping;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default, rename = "device")]
    pub devices: Vec<DeviceConfig>,
    #[serde(default, rename = "mapping")]
    pub mappings: Vec<Mapping>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceConfig {
    pub id: String,
    pub kind: DeviceKind,
    pub address: IpAddr,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    OscX32,
    OscWing,
    AhTcp,
    DliveTcp,
    AhMidi,
    YamahaDm3,
    Yamaha,
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
        assert_eq!(cfg.devices[1].kind, DeviceKind::OscX32);
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

    #[tokio::test]
    async fn watch_picks_up_a_real_file_edit() {
        let path = write_temp(EXAMPLE);
        let mut rx = watch(path.clone()).expect("failed to start watcher");

        // Initial value is available immediately without waiting on changed().
        assert_eq!(rx.borrow().mappings.len(), 1);

        let updated = EXAMPLE.replace("channel = 7", "channel = 9");
        // Give the watcher a moment to be fully registered with the OS
        // before we edit, to avoid a race on very fast filesystems.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::fs::write(&path, &updated).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), rx.changed())
            .await
            .expect("timed out waiting for watch() to notice the file edit")
            .unwrap();

        assert_eq!(rx.borrow().mappings[0].to.channel, 9);
        std::fs::remove_file(&path).ok();
    }
}

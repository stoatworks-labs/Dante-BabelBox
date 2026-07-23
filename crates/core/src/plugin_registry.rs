//! The open, host-side registry of device "kinds": some registered at
//! compile time (`register_static`, for the preamp adapters not yet
//! converted to real plugins), some loaded at runtime from a directory of
//! `.so`/`.dylib`/`.dll` files (`load_dylibs`). Both converge on one
//! `create()` lookup, replacing the old closed `DeviceKind` enum +
//! `daemon.rs::build_adapter` hardcoded `match`.
//!
//! A dylib-backed kind is wrapped in [`DylibAdapter`], which confines the
//! FFI trait object to one dedicated OS thread for its entire lifetime -
//! not because it isn't `Send` (it may or may not be, depending on the
//! plugin), but because that sidesteps needing to reason about it at all:
//! every call in and result out crosses a plain channel, so nothing here
//! needs to assert or trust that a third-party plugin's trait object
//! object is safe to move across an async runtime's worker threads. Per
//! `abi_stable`'s own stated scope ("Creating a plugin system (without
//! support for unloading)"), a loaded dylib is never unloaded for the
//! process's lifetime - only individual device instances built from it
//! are connected/disconnected.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use abi_stable::library::RootModule;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use dante_babelbox_oca::{Ono, OcaAddress, OcaEvent, OcaObjectDescriptor, OcaValue};
use dante_babelbox_oca_plugin_abi::{PluginAdapterBox, PluginRootModule_Ref, RDeviceConfig};
use tokio::sync::{broadcast, oneshot};
use tracing::{info, warn};

use crate::adapter::{AdapterError, AdapterResult, DeviceInfo};
use crate::device::DeviceConfig;
use crate::local_adapter::LocalAdapter;

type StaticCtor = Arc<dyn Fn(&DeviceConfig) -> Result<Box<dyn LocalAdapter>> + Send + Sync>;

enum RegisteredKind {
    Dylib(PluginRootModule_Ref),
    Static(StaticCtor),
}

#[derive(Default)]
pub struct PluginRegistry {
    entries: RwLock<HashMap<String, RegisteredKind>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a kind id backed by an in-process constructor - used for
    /// the vendors not yet converted to real `cdylib` plugins.
    pub fn register_static<F>(&self, kind_id: impl Into<String>, ctor: F)
    where
        F: Fn(&DeviceConfig) -> Result<Box<dyn LocalAdapter>> + Send + Sync + 'static,
    {
        self.entries.write().unwrap().insert(kind_id.into(), RegisteredKind::Static(Arc::new(ctor)));
    }

    /// Scans `dir` for plugin dynamic libraries, loading each and
    /// registering every kind id it declares. A file that fails to load
    /// (wrong ABI version, not a plugin at all, missing symbol, ...) is
    /// logged and skipped - one bad file must never bring down the host.
    /// A missing directory is likewise just logged, not an error (a
    /// deployment with no plugins yet is a valid, if minimal, setup).
    /// Returns the kind ids successfully registered.
    pub fn load_dylibs(&self, dir: &Path) -> Vec<String> {
        let mut loaded = Vec::new();
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                info!(dir = %dir.display(), error = %e, "no plugins directory to scan");
                return loaded;
            }
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !is_dylib_candidate(&path) {
                continue;
            }
            match PluginRootModule_Ref::load_from_file(&path) {
                Ok(root) => {
                    let plugin_info = root.plugin_info()();
                    let kinds: Vec<String> =
                        plugin_info.supported_kinds.iter().map(|k| k.to_string()).collect();
                    let mut entries = self.entries.write().unwrap();
                    for kind in &kinds {
                        entries.insert(kind.clone(), RegisteredKind::Dylib(root));
                    }
                    info!(
                        path = %path.display(),
                        name = %plugin_info.name,
                        vendor = %plugin_info.vendor,
                        kinds = ?kinds,
                        "loaded plugin"
                    );
                    loaded.extend(kinds);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to load plugin - skipping");
                }
            }
        }
        loaded
    }

    /// Every kind id currently registered, static or dylib alike.
    pub fn known_kinds(&self) -> Vec<String> {
        self.entries.read().unwrap().keys().cloned().collect()
    }

    pub fn create(&self, kind: &str, config: &DeviceConfig) -> Result<Box<dyn LocalAdapter>> {
        let dylib_root = {
            let entries = self.entries.read().unwrap();
            match entries.get(kind) {
                Some(RegisteredKind::Static(ctor)) => return ctor(config),
                Some(RegisteredKind::Dylib(root)) => *root,
                None => bail!("no plugin registered for kind '{kind}'"),
            }
        };

        let ffi_config = to_ffi_config(config);
        let adapter = dylib_root
            .create_adapter()(ffi_config)
            .into_result()
            .map_err(|e| anyhow!("plugin failed to create adapter for kind '{kind}': {e}"))?;
        Ok(Box::new(DylibAdapter::spawn(config.id.clone(), adapter)))
    }
}

fn is_dylib_candidate(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("so") | Some("dylib") | Some("dll"))
}

fn to_ffi_config(config: &DeviceConfig) -> RDeviceConfig {
    RDeviceConfig {
        id: config.id.clone().into(),
        address: config.address.map(|a| a.to_string().into()).into(),
        port: config.port.into(),
        channels: config.channel_count().into(),
    }
}

enum Command {
    Connect(oneshot::Sender<AdapterResult<()>>),
    Disconnect(oneshot::Sender<AdapterResult<()>>),
    Identify(oneshot::Sender<AdapterResult<DeviceInfo>>),
    Get(u32, oneshot::Sender<AdapterResult<OcaValue>>),
    Set(u32, OcaValue, oneshot::Sender<AdapterResult<()>>),
}

/// Bridges one dylib-backed [`PluginAdapterBox`] into a [`LocalAdapter`],
/// confining the actual FFI trait object to one dedicated OS thread for
/// its whole lifetime (see the module doc comment for why). Every
/// `LocalAdapter` method sends a [`Command`] over a plain channel and
/// awaits the reply; between commands the thread polls the plugin's
/// `poll_events()` (the FFI boundary has no async streams to push through)
/// on a short fixed interval and republishes anything it finds as an
/// [`OcaEvent`] on a normal broadcast channel, so callers see a uniform
/// push-based `subscribe()` regardless of the plugin's poll-based
/// telemetry underneath.
struct DylibAdapter {
    id: String,
    cmd_tx: std::sync::mpsc::Sender<Command>,
    descriptors: Vec<OcaObjectDescriptor>,
    event_tx: broadcast::Sender<OcaEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const POLL_INTERVAL: Duration = Duration::from_millis(50);

impl DylibAdapter {
    fn spawn(id: String, mut adapter: PluginAdapterBox) -> Self {
        let descriptors: Vec<OcaObjectDescriptor> =
            adapter.describe().into_iter().map(Into::into).collect();

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Command>();
        let (event_tx, _) = broadcast::channel(64);
        let event_tx_thread = event_tx.clone();

        let thread = std::thread::Builder::new()
            .name(format!("plugin-adapter-{id}"))
            .spawn(move || loop {
                match cmd_rx.recv_timeout(POLL_INTERVAL) {
                    Ok(Command::Connect(reply)) => {
                        let result = adapter
                            .connect()
                            .into_result()
                            .map_err(|e| AdapterError::Connection(e.into()));
                        let _ = reply.send(result);
                    }
                    Ok(Command::Disconnect(reply)) => {
                        let result = adapter
                            .disconnect()
                            .into_result()
                            .map_err(|e| AdapterError::Connection(e.into()));
                        let _ = reply.send(result);
                        break;
                    }
                    Ok(Command::Identify(reply)) => {
                        let result = adapter
                            .identify()
                            .into_result()
                            .map_err(|e| AdapterError::Protocol(e.into()))
                            .and_then(from_ffi_device_info);
                        let _ = reply.send(result);
                    }
                    Ok(Command::Get(ono, reply)) => {
                        let result = adapter
                            .get_object(ono)
                            .into_result()
                            .map(Into::into)
                            .map_err(|e| AdapterError::Protocol(e.into()));
                        let _ = reply.send(result);
                    }
                    Ok(Command::Set(ono, value, reply)) => {
                        let result = adapter
                            .set_object(ono, value.into())
                            .into_result()
                            .map_err(|e| AdapterError::Protocol(e.into()));
                        let _ = reply.send(result);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        for event in adapter.poll_events() {
                            let (device_id, object) = event.into_object();
                            let _ = event_tx_thread
                                .send(OcaEvent { address: OcaAddress::new(device_id, object.ono), object });
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            })
            .expect("spawning the plugin adapter thread");

        Self { id, cmd_tx, descriptors, event_tx, thread: Some(thread) }
    }

    async fn call<T: Send + 'static>(
        &self,
        make_cmd: impl FnOnce(oneshot::Sender<AdapterResult<T>>) -> Command,
    ) -> AdapterResult<T> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(make_cmd(tx))
            .map_err(|_| AdapterError::Connection("plugin adapter thread is gone".into()))?;
        rx.await.map_err(|_| AdapterError::Connection("plugin adapter thread dropped its reply".into()))?
    }
}

fn from_ffi_device_info(info: dante_babelbox_oca_plugin_abi::RDeviceInfo) -> AdapterResult<DeviceInfo> {
    let address = info
        .address
        .as_str()
        .parse()
        .map_err(|e| AdapterError::Protocol(format!("plugin returned an unparsable address: {e}")))?;
    Ok(DeviceInfo { vendor: info.vendor.into(), model: info.model.into(), address })
}

#[async_trait]
impl LocalAdapter for DylibAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        self.call(Command::Connect).await
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        let result = self.call(Command::Disconnect).await;
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
        result
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        self.call(Command::Identify).await
    }

    fn describe(&self) -> Vec<OcaObjectDescriptor> {
        self.descriptors.clone()
    }

    async fn get_object(&mut self, ono: Ono) -> AdapterResult<OcaValue> {
        self.call(|reply| Command::Get(ono.into(), reply)).await
    }

    async fn set_object(&mut self, ono: Ono, value: OcaValue) -> AdapterResult<()> {
        self.call(|reply| Command::Set(ono.into(), value, reply)).await
    }

    fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dante_babelbox_oca::OcaClass;

    struct MockAdapter {
        id: String,
    }

    #[async_trait]
    impl LocalAdapter for MockAdapter {
        fn id(&self) -> &str {
            &self.id
        }

        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo { vendor: "mock".into(), model: "mock".into(), address: "127.0.0.1".parse().unwrap() })
        }

        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            vec![OcaObjectDescriptor {
                ono: Ono(1),
                class: OcaClass::Gain,
                role: "Ch 1 Gain".into(),
                settable: true,
            }]
        }

        async fn get_object(&mut self, _ono: Ono) -> AdapterResult<OcaValue> {
            Ok(OcaValue::F32(0.0))
        }

        async fn set_object(&mut self, _ono: Ono, _value: OcaValue) -> AdapterResult<()> {
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            broadcast::channel(1).1
        }
    }

    fn mock_device(id: &str, kind: &str) -> DeviceConfig {
        DeviceConfig {
            id: id.into(),
            kind: kind.into(),
            address: None,
            port: None,
            is_virtual: false,
            channels: None,
        }
    }

    #[test]
    fn static_registration_is_found_by_create() {
        let registry = PluginRegistry::new();
        registry.register_static("mock-kind", |config| {
            Ok(Box::new(MockAdapter { id: config.id.clone() }) as Box<dyn LocalAdapter>)
        });

        assert_eq!(registry.known_kinds(), vec!["mock-kind".to_string()]);

        let adapter = registry.create("mock-kind", &mock_device("d1", "mock-kind")).unwrap();
        assert_eq!(adapter.id(), "d1");
    }

    #[test]
    fn create_fails_for_an_unregistered_kind() {
        let registry = PluginRegistry::new();
        let result = registry.create("nonexistent-kind", &mock_device("d1", "nonexistent-kind"));
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected an error for an unregistered kind"),
        };
        assert!(err.to_string().contains("nonexistent-kind"));
    }

    #[test]
    fn load_dylibs_skips_a_missing_directory_without_panicking() {
        let registry = PluginRegistry::new();
        let loaded = registry.load_dylibs(Path::new("/does/not/exist/at/all"));
        assert!(loaded.is_empty());
        assert!(registry.known_kinds().is_empty());
    }

    #[test]
    fn load_dylibs_skips_a_corrupt_file_without_panicking() {
        let dir = std::env::temp_dir().join(format!("dante-babelbox-plugin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bogus = dir.join("not_really_a_plugin.so");
        std::fs::write(&bogus, b"this is not an ELF/Mach-O shared library").unwrap();

        let registry = PluginRegistry::new();
        let loaded = registry.load_dylibs(&dir);
        assert!(loaded.is_empty(), "a corrupt file must be skipped, not registered");
        assert!(registry.known_kinds().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_dylib_candidate_matches_known_extensions_only() {
        assert!(is_dylib_candidate(Path::new("plugin.so")));
        assert!(is_dylib_candidate(Path::new("plugin.dylib")));
        assert!(is_dylib_candidate(Path::new("plugin.dll")));
        assert!(!is_dylib_candidate(Path::new("plugin.toml")));
        assert!(!is_dylib_candidate(Path::new("README.md")));
    }
}

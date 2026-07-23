//! An in-process device-adapter trait, generic over the OCA object model
//! rather than the old preamp-specific `gain_db`/`phantom` surface. This
//! is what the [`crate::Router`] and the web management API actually talk
//! to - every device instance presents this same shape regardless of
//! whether it's backed by a dynamically-loaded plugin
//! (`plugin_registry::DylibAdapter`) or a not-yet-migrated, statically
//! linked adapter (a small per-vendor shim wrapping the existing
//! `DeviceAdapter` impl - see each `preamp-adapter-*` crate's `LocalAdapter`
//! impl once added). `LocalAdapter` mirrors
//! `dante_babelbox_oca_plugin_abi::PluginAdapter`'s method surface in
//! plain Rust (owned types/`anyhow::Error` instead of
//! `RString`/`RResult`/`RVec`), so both kinds of adapter present one shape
//! here - only the FFI boundary (if any) differs underneath.

use async_trait::async_trait;
use dante_babelbox_oca::{Ono, OcaEvent, OcaObjectDescriptor, OcaValue};
use tokio::sync::broadcast;

use crate::adapter::{AdapterResult, DeviceInfo};

#[async_trait]
pub trait LocalAdapter: Send + Sync {
    fn id(&self) -> &str;
    async fn connect(&mut self) -> AdapterResult<()>;
    /// Stops this adapter's background work and releases its connection -
    /// same requirement as `DeviceAdapter::disconnect`, carried over so a
    /// device (plugin-backed or not) can be removed live without leaking.
    async fn disconnect(&mut self) -> AdapterResult<()>;
    async fn identify(&mut self) -> AdapterResult<DeviceInfo>;
    /// The full set of objects this device instance exposes right now.
    fn describe(&self) -> Vec<OcaObjectDescriptor>;
    async fn get_object(&mut self, ono: Ono) -> AdapterResult<OcaValue>;
    async fn set_object(&mut self, ono: Ono, value: OcaValue) -> AdapterResult<()>;
    fn subscribe(&self) -> broadcast::Receiver<OcaEvent>;
}

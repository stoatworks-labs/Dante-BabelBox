mod adapter;
pub mod channel_mapping;
pub mod channel_scheme;
mod device;
mod legacy_plugin_bridge;
mod local_adapter;
mod plugin_registry;
mod router;
mod types;

pub use adapter::{AdapterError, AdapterResult, DeviceAdapter, DeviceInfo};
pub use channel_mapping::ChannelMapping;
pub use device::{default_channel_count, DeviceConfig};
pub use legacy_plugin_bridge::LegacyPluginBridge;
pub use local_adapter::LocalAdapter;
pub use plugin_registry::PluginRegistry;
pub use router::{Mapping, Router};
pub use types::{PreampAddress, PreampEvent, PreampState};

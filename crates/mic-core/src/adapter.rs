use async_trait::async_trait;
use dante_babelbox_core::{AdapterResult, DeviceInfo};
use tokio::sync::broadcast;

use crate::types::MicEvent;

/// Implemented once per wireless-mic vendor protocol (Shure, Sennheiser,
/// ...). Unlike `dante_babelbox_core::DeviceAdapter`, this is read-heavy:
/// most fields are monitoring-only, `set_mute` is the only control write
/// every vendor here supports. `subscribe` carries telemetry the adapter
/// observes on the wire - metering ticks, battery/RF changes reported
/// unsolicited by the device, or confirmation of a command the bridge
/// itself sent.
#[async_trait]
pub trait MicAdapter: Send + Sync {
    fn id(&self) -> &str;
    async fn connect(&mut self) -> AdapterResult<()>;
    async fn identify(&mut self) -> AdapterResult<DeviceInfo>;
    async fn get_state(&mut self, channel: u16) -> AdapterResult<crate::types::MicState>;
    async fn set_mute(&mut self, channel: u16, muted: bool) -> AdapterResult<()>;
    fn subscribe(&self) -> broadcast::Receiver<MicEvent>;
}

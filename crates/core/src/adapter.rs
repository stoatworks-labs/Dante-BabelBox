use async_trait::async_trait;
use std::net::IpAddr;
use tokio::sync::broadcast;

use crate::types::{PreampEvent, PreampState};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("unsupported channel: {0}")]
    UnsupportedChannel(u16),
    #[error("protocol error: {0}")]
    Protocol(String),
}

pub type AdapterResult<T> = Result<T, AdapterError>;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vendor: String,
    pub model: String,
    pub address: IpAddr,
}

/// Implemented once per vendor protocol (A&H, X32-family OSC, Yamaha, ...).
/// `subscribe` carries state changes the adapter observes on the wire —
/// whether from a physical control surface or as confirmation of a command
/// the bridge itself sent — so the Router can propagate or, via echo
/// suppression, ignore them.
#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    fn id(&self) -> &str;
    async fn connect(&mut self) -> AdapterResult<()>;
    /// Stops this adapter's background socket/task(s) and releases the
    /// connection. Required (not a default no-op) so every adapter has to
    /// deliberately implement real teardown - without it, a device
    /// "removed" from the Router would leave its listener task running
    /// and its port held forever.
    async fn disconnect(&mut self) -> AdapterResult<()>;
    async fn identify(&mut self) -> AdapterResult<DeviceInfo>;
    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()>;
    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()>;
    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState>;
    fn subscribe(&self) -> broadcast::Receiver<PreampEvent>;
}

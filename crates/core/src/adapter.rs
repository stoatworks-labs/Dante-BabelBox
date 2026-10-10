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
    /// The device (or this adapter's protocol) has no such control at all -
    /// distinct from a channel out of range, so a caller can tell "this desk
    /// can't do trim" from "trim 99 doesn't exist".
    #[error("not supported: {0}")]
    Unsupported(String),
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

    /// How many input channels this adapter can trim - the digital trim (or
    /// "digital gain") on a desk's processing channel, which is what a
    /// follower desk moves for gain compensation. Numbered by the desk's own
    /// input channels, which is a different space from the preamp `channel`
    /// the methods above take (an X32 headamp index or a dLive socket is not
    /// a channel strip). `0`, the default, means no trim control.
    fn trim_inputs(&self) -> u16 {
        0
    }

    async fn get_trim(&mut self, input: u16) -> AdapterResult<f32> {
        let _ = input;
        Err(AdapterError::Unsupported(format!("device '{}' has no trim control", self.id())))
    }

    async fn set_trim(&mut self, input: u16, trim_db: f32) -> AdapterResult<()> {
        let _ = (input, trim_db);
        Err(AdapterError::Unsupported(format!("device '{}' has no trim control", self.id())))
    }

    /// Moves trim by `delta_db` from whatever the desk holds *now*, and
    /// returns the change actually made once the desk's range and step are
    /// applied (so `0.0` at a limit, or for a delta under half a step).
    ///
    /// Relative on purpose: someone at the follower desk may have moved the
    /// trim since the bridge last touched it, and that move must survive.
    /// Use the protocol's own increment/decrement where it has one; otherwise
    /// read the live value immediately before writing - never a cached one a
    /// missed push could have left stale. See [`crate::DbLaw::nudge`].
    async fn nudge_trim(&mut self, input: u16, delta_db: f32) -> AdapterResult<f32> {
        let _ = (input, delta_db);
        Err(AdapterError::Unsupported(format!("device '{}' has no trim control", self.id())))
    }
}

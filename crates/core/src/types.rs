use serde::{Deserialize, Serialize};

/// Identifies a single preamp-bearing channel on a specific device.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreampAddress {
    #[serde(rename = "device")]
    pub device_id: String,
    pub channel: u16,
}

impl PreampAddress {
    pub fn new(device_id: impl Into<String>, channel: u16) -> Self {
        Self {
            device_id: device_id.into(),
            channel,
        }
    }
}

/// pad is `None` for devices/channels that don't expose a pad switch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PreampState {
    pub gain_db: f32,
    pub phantom: bool,
    pub pad: Option<bool>,
}

/// A state change originating from a device (physical knob turn, on-screen
/// UI edit, or confirmation of a command the bridge itself issued).
#[derive(Debug, Clone)]
pub struct PreampEvent {
    pub address: PreampAddress,
    pub state: PreampState,
}

use serde::{Deserialize, Serialize};

/// Identifies a single wireless-mic receiver channel on a specific device.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MicAddress {
    #[serde(rename = "device")]
    pub device_id: String,
    pub channel: u16,
}

impl MicAddress {
    pub fn new(device_id: impl Into<String>, channel: u16) -> Self {
        Self {
            device_id: device_id.into(),
            channel,
        }
    }
}

/// Which diversity antenna is currently active, if the device reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntennaDiversity {
    A,
    B,
    /// Neither antenna is receiving a signal (squelched).
    Inactive,
}

/// Telemetry for one wireless-mic channel. Every field is `Option` because
/// not every vendor/model reports every value (e.g. AA-battery transmitters
/// don't report `battery_percent`, and RF/audio levels are only populated
/// once an adapter has metering turned on).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MicState {
    pub battery_percent: Option<u8>,
    pub battery_minutes_remaining: Option<u16>,
    /// RF signal strength in dBm. Vendors that report a raw 0-N scale
    /// instead of dBm convert it in their adapter, per that vendor's
    /// documented formula (cited in the adapter's module doc comment).
    pub rf_level_dbm: Option<i16>,
    /// Metered audio level. Not a calibrated dBFS value - the scale is
    /// whatever the vendor's own meter reports (see the adapter's module
    /// doc comment for that vendor's exact range).
    pub audio_level: Option<u8>,
    pub muted: bool,
    pub frequency_mhz: Option<f64>,
    pub antenna: Option<AntennaDiversity>,
}

/// A telemetry update originating from a device (a meter tick, a battery
/// level changing, a mute button press, or confirmation of a command the
/// bridge itself issued).
#[derive(Debug, Clone)]
pub struct MicEvent {
    pub address: MicAddress,
    pub state: MicState,
}

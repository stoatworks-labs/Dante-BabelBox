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
    /// RF signal strength in dBm. Vendors that report a raw scale instead
    /// convert it in their adapter, per that vendor's documented formula
    /// (cited in the adapter's module doc comment).
    pub rf_level_dbm: Option<f32>,
    /// RF signal quality as a 0-100 percentage, for vendors that expose a
    /// distinct quality indicator alongside raw signal strength (e.g.
    /// Sennheiser's RSQI). `None` for vendors that only report level.
    pub rf_quality_percent: Option<u8>,
    /// Audio level in calibrated dBFS. Only populated when the vendor's
    /// own spec documents it as genuine dBFS (e.g. Sennheiser's `/m/rxN/af`).
    /// Vendors that expose an uncalibrated raw meter with no documented
    /// dBFS formula (e.g. Shure's SAMPLE `eee`, 0-50) leave this `None`
    /// rather than fabricating a conversion - see that adapter's module
    /// doc comment.
    pub audio_level_dbfs: Option<f32>,
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

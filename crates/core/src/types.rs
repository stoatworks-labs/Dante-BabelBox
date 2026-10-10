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

impl PreampState {
    /// The cache entry for a channel nothing has been read from yet. Gain is
    /// NaN, so it can't pass for a reading of 0 dB.
    ///
    /// An adapter creates a channel's entry on whichever message arrives
    /// first. When that was a phantom report, the entry used to hold
    /// `gain_db: 0.0`, and `get_state` answered from it - so a "read" of a
    /// gain nobody had read returned 0 dB. The gain lock takes its reference
    /// from such a read: locking on that 0 dB would make the leader's first
    /// real report, say 32 dB, look like a 32 dB move and pin every
    /// follower's trim at its floor. `get_state` answers from the cache only
    /// once [`gain_known`](Self::gain_known); otherwise it asks the device.
    pub const UNREAD: Self = Self { gain_db: f32::NAN, phantom: false, pad: None };

    /// Whether `gain_db` is a reading rather than [`UNREAD`](Self::UNREAD)'s
    /// placeholder.
    pub fn gain_known(&self) -> bool {
        !self.gain_db.is_nan()
    }
}

/// Which fields of a [`PreampState`] an event actually carries.
///
/// A device message names ONE field: a gain knob moved, or 48 V was toggled.
/// The adapter merges it into a whole-channel `PreampState` because that is the
/// useful thing to hold, but the rest of that struct is whatever was last known
/// — and on first contact it is the default, `{ gain_db: 0.0, phantom: false }`,
/// which is not a reading of anything.
///
/// Relaying the whole struct therefore invented values. The first gain message
/// on a channel whose phantom had never been read pushed `Phantom = false` to
/// the mapped peer — dropping 48 V on a live condenser because someone nudged a
/// gain knob. The reverse pushed `Gain = 0.0 dB`. Nothing downstream could tell
/// the difference: `Router::handle_event` forwards every field of every event,
/// and echo suppression only swallows an exact echo of what the Router itself
/// last wrote.
///
/// So an event says what it actually observed, and only that is forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangedFields {
    pub gain: bool,
    pub phantom: bool,
    pub pad: bool,
}

impl ChangedFields {
    pub const NONE: Self = Self { gain: false, phantom: false, pad: false };
    pub const GAIN: Self = Self { gain: true, phantom: false, pad: false };
    pub const PHANTOM: Self = Self { gain: false, phantom: true, pad: false };
    pub const PAD: Self = Self { gain: false, phantom: false, pad: true };
    /// For a snapshot that genuinely read every field in one go.
    pub const ALL: Self = Self { gain: true, phantom: true, pad: true };

    pub const fn any(self) -> bool {
        self.gain || self.phantom || self.pad
    }
}

/// A state change originating from a device (physical knob turn, on-screen
/// UI edit, or confirmation of a command the bridge itself issued).
#[derive(Debug, Clone)]
pub struct PreampEvent {
    pub address: PreampAddress,
    pub state: PreampState,
    /// The fields this message actually reported. See [`ChangedFields`] — the
    /// rest of `state` is carried context, not a reading, and must not be
    /// relayed to a mapped peer.
    pub changed: ChangedFields,
}

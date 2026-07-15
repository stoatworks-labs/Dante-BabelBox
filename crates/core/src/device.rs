//! The config-level description of a device: what protocol it speaks (or
//! will speak, for a not-yet-built emulation), where to reach it, and how
//! many preamp channels it has. Lives in `core` (rather than the `cli`
//! crate, where it originated) because `crates/web`'s patch-bay UI needs
//! this same metadata to draw a device's rack strip, and a UI crate
//! depending on the binary crate would invert Cargo's dependency
//! direction.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    OscX32,
    OscWing,
    AhTcp,
    DliveTcp,
    AhMidi,
    YamahaDm3,
    Yamaha,
}

/// The number of preamp channels each protocol family exposes, used to
/// size a device's rack-strip in the patch-bay UI when a config entry
/// doesn't give an explicit `channels` override. `None` for protocols
/// with no implemented adapter and no documented channel count to draw
/// from (`AhMidi`/`Yamaha`) - a device declaring one of those kinds needs
/// an explicit `channels` value instead of a guessed default.
pub fn default_channel_count(kind: DeviceKind) -> Option<u16> {
    match kind {
        DeviceKind::OscX32 => Some(24),
        DeviceKind::OscWing => Some(8),
        DeviceKind::AhTcp => Some(64),
        DeviceKind::DliveTcp => Some(128),
        DeviceKind::YamahaDm3 => Some(16),
        DeviceKind::AhMidi | DeviceKind::Yamaha => None,
    }
}

/// A device the bridge knows about, real or virtual. A real device has a
/// live adapter dialing `address`/`port`; a virtual device is a
/// placeholder for the not-yet-built native device-emulation layer - it
/// has no network endpoint of its own yet, and exists only so it can
/// already be mapped against real devices' channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub id: String,
    pub kind: DeviceKind,
    pub address: Option<IpAddr>,
    pub port: Option<u16>,
    #[serde(default, rename = "virtual")]
    pub is_virtual: bool,
    #[serde(default)]
    pub channels: Option<u16>,
}

impl DeviceConfig {
    /// The channel count to use for this device: its own explicit
    /// override if set, otherwise `kind`'s documented default.
    pub fn channel_count(&self) -> Option<u16> {
        self.channels.or_else(|| default_channel_count(self.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_count_covers_every_kind() {
        assert_eq!(default_channel_count(DeviceKind::OscX32), Some(24));
        assert_eq!(default_channel_count(DeviceKind::OscWing), Some(8));
        assert_eq!(default_channel_count(DeviceKind::AhTcp), Some(64));
        assert_eq!(default_channel_count(DeviceKind::DliveTcp), Some(128));
        assert_eq!(default_channel_count(DeviceKind::YamahaDm3), Some(16));
        assert_eq!(default_channel_count(DeviceKind::AhMidi), None);
        assert_eq!(default_channel_count(DeviceKind::Yamaha), None);
    }

    #[test]
    fn channel_count_prefers_explicit_override() {
        let device = DeviceConfig {
            id: "custom".into(),
            kind: DeviceKind::OscX32,
            address: None,
            port: None,
            is_virtual: true,
            channels: Some(4),
        };
        assert_eq!(device.channel_count(), Some(4));
    }

    #[test]
    fn channel_count_falls_back_to_kind_default() {
        let device = DeviceConfig {
            id: "x32-foh".into(),
            kind: DeviceKind::OscX32,
            address: Some("10.0.0.1".parse().unwrap()),
            port: None,
            is_virtual: false,
            channels: None,
        };
        assert_eq!(device.channel_count(), Some(24));
    }
}

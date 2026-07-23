//! The config-level description of a device: what protocol it speaks (or
//! will speak, for a not-yet-built emulation), where to reach it, and how
//! many preamp channels it has. Lives in `core` (rather than the `cli`
//! crate, where it originated) because `crates/web`'s patch-bay UI needs
//! this same metadata to draw a device's rack strip, and a UI crate
//! depending on the binary crate would invert Cargo's dependency
//! direction.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// The number of preamp channels each of the five "known" (not-yet-plugin,
/// still statically-registered - see `plugin_registry`) protocol families
/// exposes, used to size a device's rack-strip in the patch-bay UI when a
/// config entry doesn't give an explicit `channels` override. Kept as a
/// small fallback table keyed by kind-id string (rather than the closed
/// `DeviceKind` enum this replaced) so a virtual device or a device whose
/// plugin hasn't been loaded yet can still get a sensible default; a real
/// plugin-backed device reports its own channel count via its adapter's
/// `describe()` once connected, which takes precedence in practice. `None`
/// for kind ids with no documented channel count to draw from (`ah-midi`,
/// `yamaha`, or any kind id not in this table at all) - a device using one
/// of those needs an explicit `channels` value instead of a guessed
/// default.
pub fn default_channel_count(kind: &str) -> Option<u16> {
    match kind {
        "osc-x32" => Some(24),
        "osc-wing" => Some(8),
        "ah-tcp" => Some(64),
        "dlive-tcp" => Some(128),
        "yamaha-dm3" => Some(16),
        _ => None,
    }
}

/// A device the bridge knows about, real or virtual. A real device has a
/// live adapter dialing `address`/`port`; a virtual device is a
/// placeholder for the not-yet-built native device-emulation layer - it
/// has no network endpoint of its own yet, and exists only so it can
/// already be mapped against real devices' channels.
///
/// `kind` is an open string ("osc-x32", "ah-tcp", ...) rather than a
/// closed enum - the plugin registry (see `plugin_registry`) is
/// inherently open-set, since a dynamically-loaded plugin can declare a
/// kind id nothing in this codebase knew about at compile time. The
/// kebab-case strings match exactly what the old `DeviceKind` enum's
/// `#[serde(rename_all = "kebab-case")]` already produced, so existing
/// `bridge.toml` files don't need editing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub id: String,
    pub kind: String,
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
        self.channels.or_else(|| default_channel_count(&self.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_count_covers_every_known_kind() {
        assert_eq!(default_channel_count("osc-x32"), Some(24));
        assert_eq!(default_channel_count("osc-wing"), Some(8));
        assert_eq!(default_channel_count("ah-tcp"), Some(64));
        assert_eq!(default_channel_count("dlive-tcp"), Some(128));
        assert_eq!(default_channel_count("yamaha-dm3"), Some(16));
        assert_eq!(default_channel_count("ah-midi"), None);
        assert_eq!(default_channel_count("yamaha"), None);
    }

    #[test]
    fn default_channel_count_is_none_for_an_unknown_kind_rather_than_a_guess() {
        assert_eq!(default_channel_count("some-future-plugin-kind"), None);
    }

    #[test]
    fn channel_count_prefers_explicit_override() {
        let device = DeviceConfig {
            id: "custom".into(),
            kind: "osc-x32".into(),
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
            kind: "osc-x32".into(),
            address: Some("10.0.0.1".parse().unwrap()),
            port: None,
            is_virtual: false,
            channels: None,
        };
        assert_eq!(device.channel_count(), Some(24));
    }
}

//! The config-level description of a device: what protocol it speaks (or
//! will speak, for a not-yet-built emulation), where to reach it, and how
//! many preamp channels it has. Lives in `core` (rather than the `cli`
//! crate, where it originated) because `crates/web`'s patch-bay UI needs
//! this same metadata to draw a device's rack strip, and a UI crate
//! depending on the binary crate would invert Cargo's dependency
//! direction.

use std::collections::BTreeMap;
use std::fmt;
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
        "osc-x32" => Some(128),
        "osc-wing" => Some(8),
        "ah-tcp" => Some(64),
        "dlive-tcp" => Some(128),
        "yamaha-dm3" => Some(16),
        "sennheiser-ewg3" => Some(1),
        "yamaha-rio-emulated" => Some(32),
        _ => None,
    }
}

/// A device the bridge knows about, real or virtual. A real device has a
/// live adapter dialing `address`/`port`; a virtual device is a
/// placeholder - it has no adapter and no network endpoint, and exists
/// only so it can already be mapped against real devices' channels.
///
/// A virtual device is not an *emulated* one. An emulated device (kind
/// `yamaha-rio-emulated`) is real as far as this struct is concerned: it
/// has an adapter and is on the network, with `address` naming the local
/// interface it is presented on instead of somewhere to dial.
///
/// `kind` is an open string ("osc-x32", "ah-tcp", ...) rather than a
/// closed enum - the plugin registry (see `plugin_registry`) is
/// inherently open-set, since a dynamically-loaded plugin can declare a
/// kind id nothing in this codebase knew about at compile time. The
/// kebab-case strings match exactly what the old `DeviceKind` enum's
/// `#[serde(rename_all = "kebab-case")]` already produced, so existing
/// `bridge.toml` files don't need editing.
///
/// Every key in a `[[device]]` table that isn't one of the named fields
/// lands in `options` (`unit_id = 5`, `model = "Rio3224-D2"`, ...). The
/// host doesn't interpret those: they are passed through to the device's
/// plugin as strings, and a plugin that doesn't know a key ignores it.
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
    /// The per-device settings only this device's plugin understands - see
    /// the struct doc comment.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, OptionValue>,
}

impl DeviceConfig {
    /// The channel count to use for this device: its own explicit
    /// override if set, otherwise `kind`'s documented default.
    pub fn channel_count(&self) -> Option<u16> {
        self.channels.or_else(|| default_channel_count(&self.kind))
    }

    /// The value of the extra `[[device]]` key named `key`, or `None` if
    /// the config didn't set it.
    pub fn option(&self, key: &str) -> Option<&OptionValue> {
        self.options.get(key)
    }
}

/// The value of one extra `[[device]]` key (see [`DeviceConfig::options`]).
/// Scalars only: a plugin receives each value as a string, so there is
/// nothing an array or a table could be rendered as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionValue {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

/// The plain value, as a plugin receives it: `true`, `5`, `1.5`, and a
/// string without its quotes.
impl fmt::Display for OptionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::String(v) => f.write_str(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_count_covers_every_known_kind() {
        assert_eq!(default_channel_count("osc-x32"), Some(128));
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
            options: Default::default(),
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
            options: Default::default(),
        };
        assert_eq!(device.channel_count(), Some(128));
    }

    /// The shape `bridge.toml` has: an array of `[[device]]` tables.
    #[derive(Debug, Serialize, Deserialize)]
    struct DeviceList {
        device: Vec<DeviceConfig>,
    }

    const DEVICE_WITH_OPTIONS: &str = r#"
        [[device]]
        id = "rio-stage"
        kind = "yamaha-mbc"
        address = "169.254.1.5"
        port = 50000
        virtual = true
        channels = 32
        unit_id = 5
        model = "Rio3224-D2"
        mac = "02:00:00:12:34:56"
        primary = true
        trim_db = 1.5
    "#;

    fn only_device(toml_text: &str) -> DeviceConfig {
        let mut list: DeviceList = toml::from_str(toml_text).unwrap();
        assert_eq!(list.device.len(), 1);
        list.device.remove(0)
    }

    #[test]
    fn extra_device_keys_land_in_options_beside_the_named_fields() {
        let device = only_device(DEVICE_WITH_OPTIONS);

        assert_eq!(device.id, "rio-stage");
        assert_eq!(device.kind, "yamaha-mbc");
        assert_eq!(device.address, Some("169.254.1.5".parse().unwrap()));
        assert_eq!(device.port, Some(50000));
        assert!(device.is_virtual);
        assert_eq!(device.channels, Some(32));

        assert_eq!(device.option("unit_id"), Some(&OptionValue::Integer(5)));
        assert_eq!(device.option("model"), Some(&OptionValue::String("Rio3224-D2".into())));
        assert_eq!(device.option("mac"), Some(&OptionValue::String("02:00:00:12:34:56".into())));
        assert_eq!(device.option("primary"), Some(&OptionValue::Bool(true)));
        assert_eq!(device.option("trim_db"), Some(&OptionValue::Float(1.5)));
        assert_eq!(device.option("not-set"), None);

        // Only the extra keys - a named field never shows up twice.
        let keys: Vec<&str> = device.options.keys().map(String::as_str).collect();
        assert_eq!(keys, ["mac", "model", "primary", "trim_db", "unit_id"]);
    }

    #[test]
    fn option_values_render_as_the_plain_value() {
        assert_eq!(OptionValue::Bool(true).to_string(), "true");
        assert_eq!(OptionValue::Integer(5).to_string(), "5");
        assert_eq!(OptionValue::Integer(-12).to_string(), "-12");
        assert_eq!(OptionValue::Float(1.5).to_string(), "1.5");
        assert_eq!(OptionValue::String("Rio3224-D2".into()).to_string(), "Rio3224-D2");
    }

    #[test]
    fn a_device_without_extra_keys_has_no_options_and_serializes_none() {
        let device = only_device(
            r#"
            [[device]]
            id = "x32-foh"
            kind = "osc-x32"
            address = "10.0.0.1"
            port = 10023
            channels = 32
            "#,
        );
        assert!(device.options.is_empty());
        assert!(!device.is_virtual);

        let written = toml::to_string(&device).unwrap();
        let table: toml::Table = toml::from_str(&written).unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["address", "channels", "id", "kind", "port", "virtual"]);
    }

    #[test]
    fn unset_optional_fields_are_still_left_out_when_serializing() {
        let device = only_device(
            r#"
            [[device]]
            id = "stage"
            kind = "osc-x32"
            virtual = true
            "#,
        );
        assert_eq!(device.address, None);
        assert_eq!(device.port, None);
        assert_eq!(device.channels, None);

        let written = toml::to_string(&device).unwrap();
        let table: toml::Table = toml::from_str(&written).unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["id", "kind", "virtual"]);
    }

    #[test]
    fn options_round_trip_through_toml() {
        let list: DeviceList = toml::from_str(DEVICE_WITH_OPTIONS).unwrap();
        let written = toml::to_string(&list).unwrap();
        assert!(written.contains("[[device]]"), "expected an array of tables, got:\n{written}");

        let before = &list.device[0];
        let after = only_device(&written);
        assert_eq!(after.id, before.id);
        assert_eq!(after.kind, before.kind);
        assert_eq!(after.address, before.address);
        assert_eq!(after.port, before.port);
        assert_eq!(after.is_virtual, before.is_virtual);
        assert_eq!(after.channels, before.channels);
        assert_eq!(after.options, before.options);
        assert_eq!(after.options.len(), 5);
    }

    #[test]
    fn a_whole_number_float_option_stays_a_float_through_toml() {
        let device = only_device(
            r#"
            [[device]]
            id = "d"
            kind = "k"
            virtual = true
            trim_db = 2.0
            "#,
        );
        assert_eq!(device.option("trim_db"), Some(&OptionValue::Float(2.0)));

        let again: DeviceConfig = toml::from_str(&toml::to_string(&device).unwrap()).unwrap();
        assert_eq!(again.option("trim_db"), Some(&OptionValue::Float(2.0)));
    }

    #[test]
    fn a_non_scalar_extra_key_is_refused_rather_than_dropped() {
        let result: Result<DeviceList, _> = toml::from_str(
            r#"
            [[device]]
            id = "d"
            kind = "k"
            virtual = true
            macs = ["02:00:00:00:00:01", "02:00:00:00:00:02"]
            "#,
        );
        assert!(result.is_err());
    }
}

//! The shared "three objects per channel" (gain, phantom, pad) `Ono`
//! scheme used by any device whose control surface is just those three
//! per channel: every preamp vendor wrapped in
//! [`crate::LegacyPluginBridge`] (Wing, AHM, dLive, DM3), and virtual
//! devices - which have no real adapter to ask, but still need a
//! plausible descriptor set so they can be mapped against real devices in
//! the patch-bay UI ahead of the still-unbuilt device-emulation layer.
//! Centralized here (rather than duplicated per plugin) so all of them
//! stay in sync automatically.

use dante_babelbox_oca::{Ono, OcaClass, OcaObjectDescriptor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Gain,
    Phantom,
    Pad,
}

impl Field {
    pub fn role_suffix(self) -> &'static str {
        match self {
            Field::Gain => "Gain",
            Field::Phantom => "Phantom",
            Field::Pad => "Pad",
        }
    }
}

pub fn gain_ono(channel: u16) -> Ono {
    Ono(3 * (channel as u32 - 1) + 1)
}

pub fn phantom_ono(channel: u16) -> Ono {
    Ono(3 * (channel as u32 - 1) + 2)
}

pub fn pad_ono(channel: u16) -> Ono {
    Ono(3 * (channel as u32 - 1) + 3)
}

/// Decodes an `Ono` back to its channel and field, `None` if it's out of
/// range for a device with this many channels.
pub fn decode_ono(ono: Ono, channels: u16) -> Option<(u16, Field)> {
    let n = ono.0.checked_sub(1)?;
    let channel = (n / 3 + 1) as u16;
    if channel == 0 || channel > channels {
        return None;
    }
    let field = match n % 3 {
        0 => Field::Gain,
        1 => Field::Phantom,
        _ => Field::Pad,
    };
    Some((channel, field))
}

/// The human role string a channel-level mapping resolver matches by name -
/// e.g. `"Ch 3 Gain"`. The single source of truth for this format, shared
/// by every descriptor producer (the legacy shim, the x32 plugin, virtual
/// devices) and every consumer (`crate::channel_mapping::resolve`).
pub fn role(channel: u16, field: Field) -> String {
    format!("Ch {channel} {}", field.role_suffix())
}

pub fn descriptor(channel: u16, field: Field) -> OcaObjectDescriptor {
    match field {
        Field::Gain => OcaObjectDescriptor {
            ono: gain_ono(channel),
            class: OcaClass::Gain,
            role: role(channel, field),
            settable: true,
        },
        Field::Phantom => OcaObjectDescriptor {
            ono: phantom_ono(channel),
            class: OcaClass::Switch,
            role: role(channel, field),
            settable: true,
        },
        Field::Pad => OcaObjectDescriptor {
            ono: pad_ono(channel),
            class: OcaClass::Switch,
            role: role(channel, field),
            settable: false,
        },
    }
}

/// Trim objects sit in their own `Ono` range, clear of the three-per-channel
/// preamp objects above: a trim belongs to a desk's *input channel*, which is
/// numbered independently of its preamps (X32 input channel 1 may be fed by
/// headamp 33), so the two can't share one channel index.
pub const TRIM_ONO_BASE: u32 = 0x0001_0000;

pub fn trim_ono(input: u16) -> Ono {
    Ono(TRIM_ONO_BASE + input as u32)
}

/// Decodes a trim `Ono` back to its input channel, `None` if it isn't one or
/// is out of range for a device with this many trimmable inputs.
pub fn decode_trim_ono(ono: Ono, inputs: u16) -> Option<u16> {
    let input = ono.0.checked_sub(TRIM_ONO_BASE)?;
    if input == 0 || input > inputs as u32 {
        return None;
    }
    Some(input as u16)
}

/// `"In 7 Trim"` - the role string the gain lock resolves a follower's trim
/// by, the same way `channel_mapping` resolves `"Ch 3 Gain"`.
pub fn trim_role(input: u16) -> String {
    format!("In {input} Trim")
}

pub fn trim_descriptor(input: u16) -> OcaObjectDescriptor {
    OcaObjectDescriptor { ono: trim_ono(input), class: OcaClass::Gain, role: trim_role(input), settable: true }
}

pub fn trim_descriptors(inputs: u16) -> Vec<OcaObjectDescriptor> {
    (1..=inputs).map(trim_descriptor).collect()
}

/// The full descriptor set for a device with this many channels - three
/// objects (gain, phantom, pad) per channel.
pub fn descriptors_for_channels(channels: u16) -> Vec<OcaObjectDescriptor> {
    let mut out = Vec::with_capacity(channels as usize * 3);
    for channel in 1..=channels {
        out.push(descriptor(channel, Field::Gain));
        out.push(descriptor(channel, Field::Phantom));
        out.push(descriptor(channel, Field::Pad));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ono_encoding_round_trips_across_channels_and_fields() {
        for channel in 1..=16u16 {
            assert_eq!(decode_ono(gain_ono(channel), 16), Some((channel, Field::Gain)));
            assert_eq!(decode_ono(phantom_ono(channel), 16), Some((channel, Field::Phantom)));
            assert_eq!(decode_ono(pad_ono(channel), 16), Some((channel, Field::Pad)));
        }
    }

    #[test]
    fn decode_ono_rejects_channels_outside_the_device_range() {
        assert_eq!(decode_ono(gain_ono(9), 8), None);
        assert_eq!(decode_ono(Ono(0), 8), None);
    }

    #[test]
    fn trim_onos_round_trip_and_never_collide_with_preamp_objects() {
        for input in 1..=40u16 {
            assert_eq!(decode_trim_ono(trim_ono(input), 40), Some(input));
            assert_eq!(decode_ono(trim_ono(input), 128), None, "a trim ono must not decode as a preamp object");
        }
        assert_eq!(decode_trim_ono(trim_ono(41), 40), None);
        assert_eq!(decode_trim_ono(gain_ono(1), 40), None);
        assert_eq!(decode_trim_ono(Ono(TRIM_ONO_BASE), 40), None);
        assert_eq!(trim_descriptor(7).role, "In 7 Trim");
    }

    #[test]
    fn descriptors_for_channels_covers_every_channel_with_three_objects_each() {
        let descriptors = descriptors_for_channels(4);
        assert_eq!(descriptors.len(), 12);
        assert!(descriptors.iter().any(|d| d.role == "Ch 2 Gain" && d.settable));
        assert!(descriptors.iter().any(|d| d.role == "Ch 4 Pad" && !d.settable));
    }
}

//! The shared "three objects per channel" (gain, phantom, pad) `Ono`
//! scheme used by any device whose control surface is just those three
//! per channel: the legacy (not-yet-plugin) preamp adapters via
//! [`crate::LegacyPreampShim`], and virtual devices - which have no real
//! adapter to ask, but still need a plausible descriptor set so they can
//! be mapped against real devices in the patch-bay UI ahead of the
//! still-unbuilt device-emulation layer. Centralized here (rather than
//! duplicated in the shim and the virtual-device path) so both stay in
//! sync automatically.

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
    fn descriptors_for_channels_covers_every_channel_with_three_objects_each() {
        let descriptors = descriptors_for_channels(4);
        assert_eq!(descriptors.len(), 12);
        assert!(descriptors.iter().any(|d| d.role == "Ch 2 Gain" && d.settable));
        assert!(descriptors.iter().any(|d| d.role == "Ch 4 Pad" && !d.settable));
    }
}

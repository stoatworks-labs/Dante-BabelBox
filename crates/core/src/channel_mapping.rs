//! The device+channel-level mapping shape users, config files, and the
//! web API all still use (unchanged from before the OCA/plugin rework) -
//! resolved into the [`crate::Router`]'s OCA-object-level [`Mapping`]s by
//! matching each side's channel against its adapter's `describe()` role
//! strings, rather than by any shared numbering scheme between vendors.
//! A single channel-level entry can resolve to more than one `Mapping`
//! (e.g. both gain and phantom), since the old bridge always propagated a
//! channel's whole state together.

use serde::{Deserialize, Serialize};

use crate::router::Mapping;
use crate::types::PreampAddress;
use dante_babelbox_oca::{OcaAddress, OcaObjectDescriptor};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMapping {
    pub from: PreampAddress,
    pub to: PreampAddress,
    #[serde(default)]
    pub bidirectional: bool,
}

/// Resolves one [`ChannelMapping`] into zero or more OCA [`Mapping`]s,
/// one per settable object both sides' channel share by role name (e.g.
/// `"Ch 3 Gain"`). Silently skips a field neither/only one side has
/// (rather than erroring) - e.g. mapping a channel that has no phantom
/// switch on one side just doesn't propagate phantom for that pair.
pub fn resolve(
    mapping: &ChannelMapping,
    from_descriptors: &[OcaObjectDescriptor],
    to_descriptors: &[OcaObjectDescriptor],
) -> Vec<Mapping> {
    let from_role = |suffix: &str| format!("Ch {} {}", mapping.from.channel, suffix);
    let to_role = |suffix: &str| format!("Ch {} {}", mapping.to.channel, suffix);

    ["Gain", "Phantom"]
        .into_iter()
        .filter_map(|field| {
            let from_ono = from_descriptors.iter().find(|d| d.settable && d.role == from_role(field))?.ono;
            let to_ono = to_descriptors.iter().find(|d| d.settable && d.role == to_role(field))?.ono;
            Some(Mapping {
                from: OcaAddress::new(mapping.from.device_id.clone(), from_ono),
                to: OcaAddress::new(mapping.to.device_id.clone(), to_ono),
                bidirectional: mapping.bidirectional,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_scheme;

    #[test]
    fn resolves_gain_and_phantom_for_a_shared_channel() {
        let mapping = ChannelMapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 5),
            bidirectional: true,
        };
        let from_descriptors = channel_scheme::descriptors_for_channels(4);
        let to_descriptors = channel_scheme::descriptors_for_channels(8);

        let resolved = resolve(&mapping, &from_descriptors, &to_descriptors);
        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|m| m.bidirectional));
        assert_eq!(resolved[0].from, OcaAddress::new("a", channel_scheme::gain_ono(1)));
        assert_eq!(resolved[0].to, OcaAddress::new("b", channel_scheme::gain_ono(5)));
        assert_eq!(resolved[1].from, OcaAddress::new("a", channel_scheme::phantom_ono(1)));
        assert_eq!(resolved[1].to, OcaAddress::new("b", channel_scheme::phantom_ono(5)));
    }

    #[test]
    fn skips_fields_missing_on_either_side_rather_than_erroring() {
        let mapping = ChannelMapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 99), // out of range for an 8-channel device
            bidirectional: false,
        };
        let from_descriptors = channel_scheme::descriptors_for_channels(4);
        let to_descriptors = channel_scheme::descriptors_for_channels(8);

        assert!(resolve(&mapping, &from_descriptors, &to_descriptors).is_empty());
    }
}

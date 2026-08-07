//! Well-known AES70-1 class definitions.
//!
//! A member (method or property) is addressed by the inheritance depth at which
//! it is *defined* plus its index within that level, so the constants below are
//! grouped by class rather than flattened.
//!
//! Definition levels are derived, not arbitrary: `OcaRoot` is 1 and each
//! subclass adds one.

use crate::pdu::MemberId;

pub mod level {
    pub const ROOT: u16 = 1;
    pub const AGENT: u16 = 2;
    pub const WORKER: u16 = 2;
    pub const BLOCK: u16 = 3;
    pub const ACTUATOR: u16 = 3;
    pub const MANAGER: u16 = 2;
    pub const SUBSCRIPTION_MANAGER: u16 = 3;

    pub const GAIN: u16 = 4;
    pub const MUTE: u16 = 4;
    pub const SWITCH: u16 = 4;
    pub const DELAY: u16 = 4;
    pub const POLARITY: u16 = 4;
    pub const BASIC_ACTUATOR: u16 = 4;
    pub const INT32_ACTUATOR: u16 = 5;
    pub const FLOAT32_ACTUATOR: u16 = 5;
    pub const STRING_ACTUATOR: u16 = 5;
    pub const BOOLEAN_ACTUATOR: u16 = 5;

    pub const SENSOR: u16 = 3;
    pub const LEVEL_SENSOR: u16 = 4;
    pub const AUDIO_LEVEL_SENSOR: u16 = 5;
    pub const BASIC_SENSOR: u16 = 4;
    pub const BOOLEAN_SENSOR: u16 = 5;
    pub const INT32_SENSOR: u16 = 5;
    pub const STRING_SENSOR: u16 = 5;
}

/// `OcaRoot` — implemented by every object on every device.
pub mod root {
    use super::*;

    pub const GET_CLASS_IDENTIFICATION: MemberId = MemberId::new(level::ROOT, 1);
    pub const GET_LOCKABLE: MemberId = MemberId::new(level::ROOT, 2);
    pub const LOCK_TOTAL: MemberId = MemberId::new(level::ROOT, 3);
    pub const UNLOCK: MemberId = MemberId::new(level::ROOT, 4);
    pub const GET_ROLE: MemberId = MemberId::new(level::ROOT, 5);

    pub const PROP_CLASS_ID: MemberId = MemberId::new(level::ROOT, 1);
    pub const PROP_OBJECT_NUMBER: MemberId = MemberId::new(level::ROOT, 3);
    pub const PROP_ROLE: MemberId = MemberId::new(level::ROOT, 5);
}

/// `OcaBlock` — the container class; the device's object tree hangs off
/// [`crate::ono::reserved::ROOT_BLOCK`].
///
/// **The method indexes here are the one place in this crate where AES70
/// revisions genuinely disagree** — `GetMembers`/`GetMembersRecursive` have not
/// sat at the same index across every published edition. Rather than pick one
/// and hope, [`crate::enumerate`] tries [`GET_MEMBERS_CANDIDATES`] in order and
/// accepts the first that isn't answered with `BadMethod`/`NotImplemented`.
/// Nothing here has been checked against a real device.
pub mod block {
    use super::*;

    pub const GET_MEMBERS: MemberId = MemberId::new(level::BLOCK, 5);
    pub const GET_MEMBERS_RECURSIVE: MemberId = MemberId::new(level::BLOCK, 6);

    /// Tried in order when enumerating; see the module comment.
    pub const GET_MEMBERS_CANDIDATES: &[MemberId] =
        &[GET_MEMBERS, MemberId::new(level::BLOCK, 4), MemberId::new(level::BLOCK, 3)];
}

/// `OcaSubscriptionManager` at ONo 4.
pub mod subscription {
    use super::*;

    pub const ADD_SUBSCRIPTION: MemberId = MemberId::new(level::SUBSCRIPTION_MANAGER, 1);
    pub const REMOVE_SUBSCRIPTION: MemberId = MemberId::new(level::SUBSCRIPTION_MANAGER, 2);
}

/// `OcaDeviceManager` at ONo 1.
pub mod device_manager {
    use super::*;

    pub const PROP_MODEL_GUID: MemberId = MemberId::new(level::MANAGER, 2);
    pub const PROP_SERIAL_NUMBER: MemberId = MemberId::new(level::MANAGER, 3);
    pub const PROP_MODEL_DESCRIPTION: MemberId = MemberId::new(level::MANAGER, 4);
    pub const PROP_DEVICE_NAME: MemberId = MemberId::new(level::MANAGER, 5);

    pub const GET_DEVICE_NAME: MemberId = MemberId::new(level::MANAGER, 5);
    pub const GET_MODEL_DESCRIPTION: MemberId = MemberId::new(level::MANAGER, 3);
}

/// Getter/setter indexes for the single-value actuator and sensor classes.
///
/// For all of these the property is index 1 at the class's own level, with
/// `Get` at method index 1 and `Set` at method index 2.
pub mod single_value {
    use super::*;

    pub const fn property(def_level: u16) -> MemberId {
        MemberId::new(def_level, 1)
    }

    pub const fn get(def_level: u16) -> MemberId {
        MemberId::new(def_level, 1)
    }

    pub const fn set(def_level: u16) -> MemberId {
        MemberId::new(def_level, 2)
    }
}

/// A class identity as reported by `OcaRoot::GetClassIdentification`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassIdentification {
    /// The class's position in the tree, e.g. `[1, 1, 1, 5]` for `OcaGain`.
    pub fields: Vec<u16>,
    pub version: u32,
}

impl ClassIdentification {
    /// Inheritance depth, which is what a property's `def_level` counts.
    pub fn depth(&self) -> u16 {
        self.fields.len() as u16
    }

    /// Best-effort human name for the common standard classes.
    ///
    /// A vendor's proprietary classes sit under its own subtree and won't
    /// resolve here — those are identified by their role names instead.
    pub fn name(&self) -> Option<&'static str> {
        Some(match self.fields.as_slice() {
            [1] => "OcaRoot",
            [1, 1] => "OcaWorker",
            [1, 2] => "OcaAgent",
            [1, 3] => "OcaManager",
            [1, 1, 1] => "OcaActuator",
            [1, 1, 2] => "OcaSensor",
            [1, 1, 3] => "OcaBlock",
            [1, 1, 1, 1] => "OcaBasicActuator",
            [1, 1, 1, 2] => "OcaMute",
            [1, 1, 1, 3] => "OcaPolarity",
            [1, 1, 1, 4] => "OcaSwitch",
            [1, 1, 1, 5] => "OcaGain",
            [1, 1, 1, 6] => "OcaDelay",
            [1, 1, 1, 1, 1] => "OcaBooleanActuator",
            [1, 1, 1, 1, 3] => "OcaInt32Actuator",
            [1, 1, 1, 1, 6] => "OcaFloat32Actuator",
            [1, 1, 1, 1, 7] => "OcaStringActuator",
            [1, 1, 2, 1] => "OcaBasicSensor",
            [1, 1, 2, 2] => "OcaLevelSensor",
            [1, 1, 2, 1, 1] => "OcaBooleanSensor",
            [1, 1, 2, 1, 3] => "OcaInt32Sensor",
            [1, 1, 2, 1, 6] => "OcaStringSensor",
            [1, 1, 2, 2, 1] => "OcaAudioLevelSensor",
            [1, 3, 1] => "OcaDeviceManager",
            [1, 3, 4] => "OcaSubscriptionManager",
            _ => return None,
        })
    }

    /// Whether this is an `OcaBlock`, i.e. a container worth recursing into.
    pub fn is_block(&self) -> bool {
        self.fields.as_slice() == [1, 1, 3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_value_accessors_follow_the_get_1_set_2_convention() {
        assert_eq!(single_value::get(level::GAIN), MemberId::new(4, 1));
        assert_eq!(single_value::set(level::GAIN), MemberId::new(4, 2));
        assert_eq!(single_value::property(level::MUTE), MemberId::new(4, 1));
    }

    #[test]
    fn class_depth_matches_the_def_level_of_its_own_properties() {
        let gain = ClassIdentification { fields: vec![1, 1, 1, 5], version: 2 };
        assert_eq!(gain.depth(), level::GAIN);
        assert_eq!(gain.name(), Some("OcaGain"));
    }

    #[test]
    fn unknown_vendor_classes_have_no_standard_name() {
        let vendor = ClassIdentification { fields: vec![1, 1, 1, 128, 3], version: 1 };
        assert_eq!(vendor.name(), None);
        assert_eq!(vendor.depth(), 5);
    }

    #[test]
    fn only_ocablock_counts_as_a_container() {
        assert!(ClassIdentification { fields: vec![1, 1, 3], version: 2 }.is_block());
        assert!(!ClassIdentification { fields: vec![1, 1, 1, 5], version: 2 }.is_block());
    }

    #[test]
    fn get_members_candidates_lead_with_the_primary_constant() {
        assert_eq!(block::GET_MEMBERS_CANDIDATES[0], block::GET_MEMBERS);
        // Every candidate is defined at OcaBlock's own level.
        assert!(block::GET_MEMBERS_CANDIDATES.iter().all(|m| m.def_level == level::BLOCK));
    }
}

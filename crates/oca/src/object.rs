use serde::{Deserialize, Serialize};

use crate::{Ono, OcaClass, OcaValue};

/// The static shape of one object: what it is, without its current value.
/// This is what an adapter's `describe()` returns - the schema the host
/// needs to draw a UI or build an Ono<->field lookup table, before any
/// value has been read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcaObjectDescriptor {
    pub ono: Ono,
    pub class: OcaClass,
    /// Human label, e.g. `"Ch 3 Gain"` or `"Battery %"`.
    pub role: String,
    /// `false` for sensor-only telemetry (battery %, RF level) that a
    /// caller can read but never write.
    pub settable: bool,
}

/// One object's descriptor plus its current value - what `get_object`
/// returns and what an [`OcaEvent`] carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcaObject {
    pub ono: Ono,
    pub class: OcaClass,
    pub role: String,
    pub settable: bool,
    pub value: OcaValue,
}

impl OcaObject {
    pub fn descriptor(&self) -> OcaObjectDescriptor {
        OcaObjectDescriptor {
            ono: self.ono,
            class: self.class,
            role: self.role.clone(),
            settable: self.settable,
        }
    }

    pub fn from_descriptor(descriptor: OcaObjectDescriptor, value: OcaValue) -> Self {
        Self {
            ono: descriptor.ono,
            class: descriptor.class,
            role: descriptor.role,
            settable: descriptor.settable,
            value,
        }
    }
}

/// Identifies a single object on a specific device - the OCA-flavoured
/// replacement for the old `PreampAddress`/`MicAddress`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OcaAddress {
    #[serde(rename = "device")]
    pub device_id: String,
    pub ono: Ono,
}

impl OcaAddress {
    pub fn new(device_id: impl Into<String>, ono: Ono) -> Self {
        Self { device_id: device_id.into(), ono }
    }
}

/// A value change - from a device (wire update, physical control surface,
/// or confirmation of a command the bridge itself issued) or an API/UI
/// edit. The OCA-flavoured replacement for the old `PreampEvent`/`MicEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcaEvent {
    pub address: OcaAddress,
    pub object: OcaObject,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_round_trips_through_an_object() {
        let descriptor = OcaObjectDescriptor {
            ono: Ono(7),
            class: OcaClass::Gain,
            role: "Ch 1 Gain".into(),
            settable: true,
        };
        let object = OcaObject::from_descriptor(descriptor.clone(), OcaValue::F32(-6.0));
        assert_eq!(object.descriptor(), descriptor);
        assert_eq!(object.value, OcaValue::F32(-6.0));
    }

    #[test]
    fn phantom_and_pad_are_modeled_as_switch_not_a_dedicated_boolean_class() {
        // Documented modeling compromise: AES70's dedicated boolean-actuator
        // class isn't consistently documented across the sources this
        // project trusts, so a 2-position OcaSwitch stands in for both
        // preamp phantom power and pad.
        let phantom = OcaObjectDescriptor {
            ono: Ono(1),
            class: OcaClass::Switch,
            role: "Ch 1 Phantom".into(),
            settable: true,
        };
        let pad = OcaObjectDescriptor {
            ono: Ono(2),
            class: OcaClass::Switch,
            role: "Ch 1 Pad".into(),
            settable: true,
        };
        assert_eq!(phantom.class, OcaClass::Switch);
        assert_eq!(pad.class, OcaClass::Switch);
    }

    #[test]
    fn antenna_diversity_is_modeled_as_a_string_sensor_not_an_enum_class() {
        // Documented modeling compromise: no 3-state enum-sensor class -
        // "A"/"B"/"Inactive" as OcaValue::String against a StringSensor
        // descriptor instead.
        let antenna = OcaObjectDescriptor {
            ono: Ono(9),
            class: OcaClass::StringSensor,
            role: "Antenna".into(),
            settable: false,
        };
        let object = OcaObject::from_descriptor(antenna, OcaValue::String("Inactive".into()));
        assert_eq!(object.value.as_str(), Some("Inactive"));
        assert!(!object.settable);
    }
}

use serde::{Deserialize, Serialize};

/// The AES70-1 classes this project's internal model actually uses.
/// Deliberately a small, curated subset of the real standard's class tree
/// (see the crate doc comment for why), not an attempt at full coverage:
///
/// - `Gain`/`Mute`/`Switch`/`Polarity`/`Delay` - actuators (settable).
/// - `BasicSensor`/`LevelSensor`/`AudioLevelSensor`/`BooleanSensor`/
///   `Int32Sensor`/`StringSensor` - sensors (read-only telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OcaClass {
    Gain,
    Mute,
    Switch,
    Polarity,
    Delay,
    BasicSensor,
    LevelSensor,
    AudioLevelSensor,
    BooleanSensor,
    Int32Sensor,
    StringSensor,
}

impl OcaClass {
    /// Sensor classes are read-only telemetry by convention; everything
    /// else is a settable actuator. Adapters still set `OcaObjectDescriptor::settable`
    /// explicitly rather than deriving it from this, but it's a useful
    /// default/sanity check.
    pub fn is_sensor(self) -> bool {
        matches!(
            self,
            OcaClass::BasicSensor
                | OcaClass::LevelSensor
                | OcaClass::AudioLevelSensor
                | OcaClass::BooleanSensor
                | OcaClass::Int32Sensor
                | OcaClass::StringSensor
        )
    }
}

/// A value carried by an [`crate::OcaObject`]. Deliberately just four
/// variants - every existing field across both the preamp (`gain_db: f32`,
/// `phantom: bool`, `pad: Option<bool>`) and mic-telemetry (`battery_percent:
/// Option<u8>`, `rf_level_dbm: Option<f32>`, `antenna: AntennaDiversity`, ...)
/// domains fits one of these without a fifth variant:
/// - `u8`/`u16`/`u32` telemetry values (battery %, battery minutes, RF
///   quality %) fit in `I32`.
/// - `frequency_mhz` (currently `f64`) is cast to `F32`, documented as a
///   deliberate precision trade-off in the crate doc comment.
/// - `AntennaDiversity`'s three states are represented as `String` (`"A"`,
///   `"B"`, `"Inactive"`) rather than inventing a dedicated enum-sensor
///   variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OcaValue {
    F32(f32),
    I32(i32),
    Bool(bool),
    String(String),
}

impl OcaValue {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            OcaValue::F32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            OcaValue::I32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            OcaValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            OcaValue::String(v) => Some(v.as_str()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_classes_are_flagged_correctly() {
        assert!(OcaClass::LevelSensor.is_sensor());
        assert!(OcaClass::AudioLevelSensor.is_sensor());
        assert!(OcaClass::StringSensor.is_sensor());
        assert!(!OcaClass::Gain.is_sensor());
        assert!(!OcaClass::Switch.is_sensor());
    }

    #[test]
    fn accessors_round_trip_the_matching_variant_only() {
        let v = OcaValue::F32(-6.0);
        assert_eq!(v.as_f32(), Some(-6.0));
        assert_eq!(v.as_bool(), None);

        let antenna = OcaValue::String("Inactive".into());
        assert_eq!(antenna.as_str(), Some("Inactive"));
        assert_eq!(antenna.as_i32(), None);
    }

    #[test]
    fn serde_round_trips_every_variant() {
        for v in [
            OcaValue::F32(614.125),
            OcaValue::I32(-6),
            OcaValue::Bool(true),
            OcaValue::String("A".into()),
        ] {
            let json = serde_json::to_string(&v).unwrap();
            let back: OcaValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }
}

//! The `PluginAdapter` implementation: connect over OCP.1, enumerate whatever
//! the device says it has, and expose it in this project's OCA model.
//!
//! Unlike every other plugin here, this one does *not* wrap a
//! `dante_babelbox_core::DeviceAdapter` through `LegacyPluginBridge`. That
//! bridge assumes a fixed channel count and a gain/phantom/pad shape known
//! ahead of time; an AES70 device's object set is only known once it has been
//! asked. So `describe()` here reflects the live device rather than its kind.
//!
//! Owns its own **multi-threaded** Tokio runtime, for the reason spelled out in
//! `LegacyPluginBridge`'s module comment: a current-thread runtime only drives
//! spawned tasks while something is inside `block_on`, and `poll_events` is
//! called by the host outside any `block_on`, so the notification task would
//! starve.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use abi_stable::std_types::{RResult, RString, RVec};
use dante_babelbox_oca::{OcaClass, OcaObject, OcaValue, Ono as HostOno};
use dante_babelbox_oca_plugin_abi::{
    OcaEventFfi, OcaObjectDescriptorFfi, OcaValueFfi, PluginAdapter, RDeviceInfo,
};
use dante_babelbox_ocp1::classes::{device_manager, level, ClassIdentification};
use dante_babelbox_ocp1::value::Reader;
use dante_babelbox_ocp1::{enumerate, Client, DiscoveredObject, Error as Ocp1Error, Ono};
use tokio::runtime::Runtime;
use tracing::{debug, warn};

use crate::roles;

/// One object we found on the device, resolved down to everything needed to
/// read it, write it and label it.
#[derive(Debug, Clone)]
struct Entry {
    ono: Ono,
    class: OcaClass,
    /// The AES70 definition level of the class, which is what addresses its
    /// `Get`/`Set` pair.
    def_level: u16,
    role: String,
    settable: bool,
    /// True when this object is presented to the host as a boolean even though
    /// the wire type isn't one — a phantom-power switch, principally.
    as_bool: bool,
}

impl Entry {
    fn descriptor(&self) -> OcaObjectDescriptorFfi {
        OcaObjectDescriptorFfi {
            ono: self.ono.0,
            class: self.class.into(),
            role: self.role.clone().into(),
            settable: self.settable,
        }
    }
}

pub struct RedNetAdapter {
    id: String,
    remote: SocketAddr,
    runtime: Runtime,
    client: Option<Arc<Client>>,
    entries: Vec<Entry>,
    events: Arc<StdMutex<VecDeque<OcaEventFfi>>>,
}

impl RedNetAdapter {
    pub fn new(id: String, remote: SocketAddr) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("building the RedNet plugin's Tokio runtime");

        Self {
            id,
            remote,
            runtime,
            client: None,
            entries: Vec::new(),
            events: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    fn entry(&self, ono: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.ono.0 == ono)
    }
}

/// Map an AES70 class identification onto this project's curated class set.
///
/// Returns `None` for anything outside it — a vendor-proprietary class, or a
/// standard one this model has no variant for. Those objects are dropped from
/// `describe()` rather than being forced into the nearest-looking variant.
fn host_class(class: &ClassIdentification) -> Option<(OcaClass, u16)> {
    Some(match class.fields.as_slice() {
        [1, 1, 1, 5] => (OcaClass::Gain, level::GAIN),
        [1, 1, 1, 2] => (OcaClass::Mute, level::MUTE),
        [1, 1, 1, 4] => (OcaClass::Switch, level::SWITCH),
        [1, 1, 1, 3] => (OcaClass::Polarity, level::POLARITY),
        [1, 1, 1, 6] => (OcaClass::Delay, level::DELAY),
        [1, 1, 1, 1, 1] => (OcaClass::Switch, level::BOOLEAN_ACTUATOR),
        [1, 1, 1, 1, 3] => (OcaClass::Int32Sensor, level::INT32_ACTUATOR),
        [1, 1, 2, 1] => (OcaClass::BasicSensor, level::BASIC_SENSOR),
        [1, 1, 2, 2] => (OcaClass::LevelSensor, level::LEVEL_SENSOR),
        [1, 1, 2, 2, 1] => (OcaClass::AudioLevelSensor, level::AUDIO_LEVEL_SENSOR),
        [1, 1, 2, 1, 1] => (OcaClass::BooleanSensor, level::BOOLEAN_SENSOR),
        [1, 1, 2, 1, 3] => (OcaClass::Int32Sensor, level::INT32_SENSOR),
        [1, 1, 2, 1, 6] => (OcaClass::StringSensor, level::STRING_SENSOR),
        _ => return None,
    })
}

/// Resolve one discovered object into an [`Entry`], or drop it.
fn to_entry(object: &DiscoveredObject) -> Option<Entry> {
    let (class, def_level) = host_class(&object.class)?;

    // Try for a host-recognised "Ch {n} Gain"/"Ch {n} Phantom" role; fall back
    // to the device's own naming, which stays fully addressable by ONo even
    // though the config-file channel shorthand won't find it.
    let canonical = roles::classify(&object.role, &object.path, class);
    let as_bool = canonical.as_deref().is_some_and(|r| r.ends_with("Phantom"))
        || matches!(class, OcaClass::Mute | OcaClass::Polarity | OcaClass::BooleanSensor);

    Some(Entry {
        ono: object.ono,
        class,
        def_level,
        role: canonical.unwrap_or_else(|| object.qualified_role()),
        settable: !class.is_sensor(),
        as_bool,
    })
}

/// Decode a property value off the wire into the host's model.
///
/// The boolean conventions here are the honest weak point: AES70 defines
/// `OcaMute::State` as 1 = Muted / 2 = Unmuted and `OcaPolarity::State` as
/// 1 = Non-inverted / 2 = Inverted, but an `OcaSwitch`'s position is just an
/// index into the vendor's own list of position names. **Treating position 0 as
/// "off" is an assumption**, and it is the assumption to check first if a
/// phantom control reads backwards against real hardware.
fn decode(entry: &Entry, bytes: &[u8]) -> Result<OcaValue, Ocp1Error> {
    let mut r = Reader::new(bytes);
    Ok(match entry.class {
        OcaClass::Gain | OcaClass::Delay | OcaClass::LevelSensor | OcaClass::AudioLevelSensor => {
            OcaValue::F32(r.f32()?)
        }
        OcaClass::Mute => OcaValue::Bool(r.u8()? == 1),
        OcaClass::Polarity => OcaValue::Bool(r.u8()? == 2),
        OcaClass::Switch if entry.as_bool => OcaValue::Bool(r.u16()? != 0),
        OcaClass::Switch => OcaValue::I32(i32::from(r.u16()?)),
        OcaClass::BooleanSensor | OcaClass::BasicSensor => OcaValue::Bool(r.u8()? != 0),
        OcaClass::Int32Sensor => OcaValue::I32(r.i32()?),
        OcaClass::StringSensor => OcaValue::String(r.string()?),
    })
}

impl RedNetAdapter {
    /// Write one value, choosing the wire type from the object's class. See
    /// [`decode`] for the boolean conventions.
    async fn write(client: &Client, entry: &Entry, value: OcaValue) -> Result<(), Ocp1Error> {
        let ono = entry.ono;
        let lvl = entry.def_level;
        match (entry.class, value) {
            (OcaClass::Gain | OcaClass::Delay, OcaValue::F32(v)) => {
                client.set_f32(ono, lvl, v).await
            }
            // Accept an integer for a float control rather than refusing a
            // perfectly unambiguous "45 dB".
            (OcaClass::Gain | OcaClass::Delay, OcaValue::I32(v)) => {
                client.set_f32(ono, lvl, v as f32).await
            }
            (OcaClass::Mute, OcaValue::Bool(v)) => client.set_u8(ono, lvl, if v { 1 } else { 2 }).await,
            (OcaClass::Polarity, OcaValue::Bool(v)) => {
                client.set_u8(ono, lvl, if v { 2 } else { 1 }).await
            }
            (OcaClass::Switch, OcaValue::Bool(v)) => client.set_u16(ono, lvl, u16::from(v)).await,
            (OcaClass::Switch, OcaValue::I32(v)) => client.set_u16(ono, lvl, v as u16).await,
            _ => Err(Ocp1Error::Status { code: 6, name: "ParameterError" }),
        }
    }
}

impl PluginAdapter for RedNetAdapter {
    fn id(&self) -> RString {
        self.id.clone().into()
    }

    fn connect(&mut self) -> RResult<(), RString> {
        let remote = self.remote;
        let connected = self.runtime.block_on(async move { Client::connect(remote).await });
        let client = match connected {
            Ok(client) => client,
            Err(e) => return RResult::RErr(format!("connecting to {remote}: {e}").into()),
        };

        let discovered = match self.runtime.block_on(enumerate::enumerate(&client)) {
            Ok(objects) => objects,
            Err(e) => return RResult::RErr(format!("enumerating {remote}: {e}").into()),
        };

        self.entries = discovered.iter().filter_map(to_entry).collect();
        debug!(
            device = %self.id,
            found = discovered.len(),
            kept = self.entries.len(),
            "enumerated an AES70 device"
        );

        // Subscribe to everything we kept, so the host sees changes made at the
        // device's front panel or by another controller.
        let by_ono: HashMap<u32, Entry> =
            self.entries.iter().map(|e| (e.ono.0, e.clone())).collect();
        for entry in &self.entries {
            if let Err(e) = self.runtime.block_on(client.subscribe(entry.ono)) {
                // A device that refuses subscriptions is still usable by
                // polling; don't fail the whole connect over it.
                debug!(ono = %entry.ono, error = %e, "subscription refused");
            }
        }

        let mut notifications = client.notifications();
        let events = Arc::clone(&self.events);
        let device_id = self.id.clone();
        self.runtime.spawn(async move {
            loop {
                match notifications.recv().await {
                    Ok(notification) => {
                        let Some(entry) = by_ono.get(&notification.emitter) else { continue };
                        // The notification carries the new value followed by a
                        // change-type byte; the decoder reads from the front
                        // and ignores the tail.
                        match decode(entry, &notification.value) {
                            Ok(value) => {
                                let object = OcaObject {
                                    ono: HostOno(entry.ono.0),
                                    class: entry.class,
                                    role: entry.role.clone(),
                                    settable: entry.settable,
                                    value,
                                };
                                events
                                    .lock()
                                    .expect("event queue mutex poisoned")
                                    .push_back(OcaEventFfi::from_event(
                                        device_id.as_str(),
                                        object,
                                    ));
                            }
                            Err(e) => warn!(ono = %entry.ono, error = %e, "undecodable notification"),
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        self.client = Some(client);
        RResult::ROk(())
    }

    fn disconnect(&mut self) -> RResult<(), RString> {
        // Dropping the client closes the TCP connection, which ends the read,
        // write and heartbeat tasks and then the notification task with them.
        self.client = None;
        self.entries.clear();
        self.events.lock().expect("event queue mutex poisoned").clear();
        RResult::ROk(())
    }

    fn identify(&mut self) -> RResult<RDeviceInfo, RString> {
        let Some(client) = self.client.clone() else {
            return RResult::RErr("not connected".into());
        };
        let manager = dante_babelbox_ocp1::ono::reserved::DEVICE_MANAGER;

        let model = self
            .runtime
            .block_on(async {
                client
                    .request(manager, device_manager::GET_MODEL_DESCRIPTION, 0, Vec::new())
                    .await
                    .and_then(|r| Reader::new(&r.params).string())
            })
            .unwrap_or_else(|e| {
                debug!(error = %e, "device would not report a model description");
                "AES70 device".to_string()
            });

        RResult::ROk(RDeviceInfo {
            vendor: "Focusrite".into(),
            model: model.into(),
            address: self.remote.to_string().into(),
        })
    }

    fn describe(&self) -> RVec<OcaObjectDescriptorFfi> {
        self.entries.iter().map(Entry::descriptor).collect::<Vec<_>>().into()
    }

    fn get_object(&mut self, ono: u32) -> RResult<OcaValueFfi, RString> {
        let Some(client) = self.client.clone() else {
            return RResult::RErr("not connected".into());
        };
        let Some(entry) = self.entry(ono).cloned() else {
            return RResult::RErr(format!("no object 0x{ono:08x} on this device").into());
        };

        let read = self.runtime.block_on(client.get(entry.ono, entry.def_level));
        match read.and_then(|bytes| decode(&entry, &bytes)) {
            Ok(value) => RResult::ROk(value.into()),
            Err(e) => RResult::RErr(format!("reading {}: {e}", entry.role).into()),
        }
    }

    fn set_object(&mut self, ono: u32, value: OcaValueFfi) -> RResult<(), RString> {
        let Some(client) = self.client.clone() else {
            return RResult::RErr("not connected".into());
        };
        let Some(entry) = self.entry(ono).cloned() else {
            return RResult::RErr(format!("no object 0x{ono:08x} on this device").into());
        };
        if !entry.settable {
            return RResult::RErr(format!("{} is read-only", entry.role).into());
        }

        match self.runtime.block_on(Self::write(&client, &entry, value.into())) {
            Ok(()) => RResult::ROk(()),
            Err(e) => RResult::RErr(format!("writing {}: {e}", entry.role).into()),
        }
    }

    fn poll_events(&mut self) -> RVec<OcaEventFfi> {
        let mut queue = self.events.lock().expect("event queue mutex poisoned");
        queue.drain(..).collect::<Vec<_>>().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(fields: &[u16]) -> ClassIdentification {
        ClassIdentification { fields: fields.to_vec(), version: 2 }
    }

    fn discovered(ono: u32, fields: &[u16], role: &str, path: &[&str]) -> DiscoveredObject {
        DiscoveredObject {
            ono: Ono(ono),
            class: class(fields),
            role: role.to_string(),
            path: path.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn gain_objects_become_settable_channel_roles() {
        let entry = to_entry(&discovered(0x1001, &[1, 1, 1, 5], "Gain", &["Input 3"])).unwrap();
        assert_eq!(entry.role, "Ch 3 Gain");
        assert_eq!(entry.class, OcaClass::Gain);
        assert_eq!(entry.def_level, level::GAIN);
        assert!(entry.settable);
        assert!(!entry.as_bool);
    }

    #[test]
    fn a_phantom_switch_is_presented_as_a_boolean() {
        let entry =
            to_entry(&discovered(0x1002, &[1, 1, 1, 4], "Phantom Power", &["Input 3"])).unwrap();
        assert_eq!(entry.role, "Ch 3 Phantom");
        assert!(entry.as_bool);
        assert_eq!(decode(&entry, &[0x00, 0x01]).unwrap(), OcaValue::Bool(true));
        assert_eq!(decode(&entry, &[0x00, 0x00]).unwrap(), OcaValue::Bool(false));
    }

    /// A switch that isn't phantom keeps its integer position — an input
    /// impedance selector has more than two states and would be destroyed by
    /// being squeezed into a bool.
    #[test]
    fn a_non_phantom_switch_keeps_its_position() {
        let entry =
            to_entry(&discovered(0x1003, &[1, 1, 1, 4], "Impedance", &["Input 3"])).unwrap();
        assert_eq!(entry.role, "Input 3/Impedance");
        assert!(!entry.as_bool);
        assert_eq!(decode(&entry, &[0x00, 0x02]).unwrap(), OcaValue::I32(2));
    }

    #[test]
    fn sensors_are_not_settable() {
        let entry =
            to_entry(&discovered(0x1004, &[1, 1, 2, 2, 1], "Level", &["Input 3"])).unwrap();
        assert_eq!(entry.class, OcaClass::AudioLevelSensor);
        assert!(!entry.settable);
        assert_eq!(decode(&entry, &(-20.0f32).to_be_bytes()).unwrap(), OcaValue::F32(-20.0));
    }

    /// An object whose class isn't in the host's curated set is dropped, not
    /// mapped to the nearest-looking variant.
    #[test]
    fn vendor_classes_are_dropped_rather_than_guessed() {
        assert!(to_entry(&discovered(0x2001, &[1, 1, 1, 128, 3], "Mystery", &[])).is_none());
        assert!(host_class(&class(&[1, 1, 1, 128, 3])).is_none());
    }

    /// An object that doesn't fit the channel shorthand still has to be
    /// addressable — it keeps the device's own name, including its path.
    #[test]
    fn unmatched_objects_keep_the_devices_own_naming() {
        let entry =
            to_entry(&discovered(0x1005, &[1, 1, 2, 1, 3], "Sample Rate", &["Device"])).unwrap();
        assert_eq!(entry.role, "Device/Sample Rate");
    }

    #[test]
    fn mute_and_polarity_follow_the_aes70_state_encodings() {
        let mute = to_entry(&discovered(0x1006, &[1, 1, 1, 2], "Mute", &[])).unwrap();
        assert_eq!(decode(&mute, &[1]).unwrap(), OcaValue::Bool(true)); // Muted
        assert_eq!(decode(&mute, &[2]).unwrap(), OcaValue::Bool(false)); // Unmuted

        let polarity = to_entry(&discovered(0x1007, &[1, 1, 1, 3], "Polarity", &[])).unwrap();
        assert_eq!(decode(&polarity, &[1]).unwrap(), OcaValue::Bool(false)); // Non-inverted
        assert_eq!(decode(&polarity, &[2]).unwrap(), OcaValue::Bool(true)); // Inverted
    }

    #[test]
    fn a_truncated_value_is_an_error_not_a_panic() {
        let entry = to_entry(&discovered(0x1008, &[1, 1, 1, 5], "Gain", &["Input 1"])).unwrap();
        assert!(decode(&entry, &[0x00, 0x01]).is_err());
    }
}

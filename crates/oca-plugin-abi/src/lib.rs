//! The FFI-safe contract between the host and a dynamically-loaded device
//! plugin. This is the `interface crate` in `abi_stable`'s terminology:
//! it declares the types/traits both the host (`user crate`) and every
//! plugin `.so`/`.dylib`/`.dll` (`implementation crate`) compile against.
//!
//! Two distinct `abi_stable` mechanisms are used here, deliberately not
//! conflated (confirmed against `abi_stable` 0.11's own readme/library
//! docs, not guessed):
//!
//! - The **root module** (```PluginRootModule```/`PluginRootModule_Ref`) is
//!   an `abi_stable` "prefix type" - a `#[repr(C)]` struct of `extern "C"
//!   fn` pointers - loaded via [`abi_stable::library::RootModule`]. This is
//!   the one thing `#[export_root_module]` exports from a plugin binary.
//! - [`PluginAdapter`] is a `#[sabi_trait]`-generated ffi-safe trait
//!   object, constructed *by* the root module's `create_adapter` function -
//!   this is what actually talks to one connected device instance.
//!
//! `host` and every plugin must be built with the same Rust toolchain
//! version at each release: `abi_stable` stabilizes the *shape* of the
//! interface (and checks it at load time, refusing to load a mismatched
//! plugin rather than risking undefined behaviour), not the underlying
//! Rust compiler ABI itself - that constraint is inherent to Rust FFI and
//! isn't solved by this crate.

// `#[sabi_trait]`'s expansion (this abi_stable_derive version, against
// current rustc) trips `non_local_definitions` on its generated `impl`
// blocks - an upstream macro-hygiene lint issue, not something fixable
// from this crate's own code. `_Ref`/`_TO` suffixed type names
// (`PluginRootModule_Ref`, `PluginAdapter_TO`, ...) are `abi_stable`'s own
// established, documented naming convention for these generated types.
#![allow(non_local_definitions, non_camel_case_types)]

use abi_stable::{
    library::RootModule,
    package_version_strings, sabi_trait,
    sabi_types::VersionStrings,
    std_types::{ROption, RResult, RString, RVec},
    StableAbi,
};

use dante_babelbox_oca::{Ono, OcaClass, OcaObject, OcaObjectDescriptor, OcaValue};

/// FFI-safe mirror of [`dante_babelbox_oca::OcaClass`].
#[repr(C)]
#[derive(StableAbi, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcaClassFfi {
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

impl From<OcaClass> for OcaClassFfi {
    fn from(class: OcaClass) -> Self {
        match class {
            OcaClass::Gain => Self::Gain,
            OcaClass::Mute => Self::Mute,
            OcaClass::Switch => Self::Switch,
            OcaClass::Polarity => Self::Polarity,
            OcaClass::Delay => Self::Delay,
            OcaClass::BasicSensor => Self::BasicSensor,
            OcaClass::LevelSensor => Self::LevelSensor,
            OcaClass::AudioLevelSensor => Self::AudioLevelSensor,
            OcaClass::BooleanSensor => Self::BooleanSensor,
            OcaClass::Int32Sensor => Self::Int32Sensor,
            OcaClass::StringSensor => Self::StringSensor,
        }
    }
}

impl From<OcaClassFfi> for OcaClass {
    fn from(class: OcaClassFfi) -> Self {
        match class {
            OcaClassFfi::Gain => Self::Gain,
            OcaClassFfi::Mute => Self::Mute,
            OcaClassFfi::Switch => Self::Switch,
            OcaClassFfi::Polarity => Self::Polarity,
            OcaClassFfi::Delay => Self::Delay,
            OcaClassFfi::BasicSensor => Self::BasicSensor,
            OcaClassFfi::LevelSensor => Self::LevelSensor,
            OcaClassFfi::AudioLevelSensor => Self::AudioLevelSensor,
            OcaClassFfi::BooleanSensor => Self::BooleanSensor,
            OcaClassFfi::Int32Sensor => Self::Int32Sensor,
            OcaClassFfi::StringSensor => Self::StringSensor,
        }
    }
}

/// FFI-safe mirror of [`dante_babelbox_oca::OcaValue`].
#[repr(C)]
#[derive(StableAbi, Debug, Clone, PartialEq)]
pub enum OcaValueFfi {
    F32(f32),
    I32(i32),
    Bool(bool),
    String(RString),
}

impl From<OcaValue> for OcaValueFfi {
    fn from(value: OcaValue) -> Self {
        match value {
            OcaValue::F32(v) => Self::F32(v),
            OcaValue::I32(v) => Self::I32(v),
            OcaValue::Bool(v) => Self::Bool(v),
            OcaValue::String(v) => Self::String(v.into()),
        }
    }
}

impl From<OcaValueFfi> for OcaValue {
    fn from(value: OcaValueFfi) -> Self {
        match value {
            OcaValueFfi::F32(v) => Self::F32(v),
            OcaValueFfi::I32(v) => Self::I32(v),
            OcaValueFfi::Bool(v) => Self::Bool(v),
            OcaValueFfi::String(v) => Self::String(v.into()),
        }
    }
}

/// FFI-safe mirror of [`dante_babelbox_oca::OcaObjectDescriptor`].
#[repr(C)]
#[derive(StableAbi, Debug, Clone, PartialEq)]
pub struct OcaObjectDescriptorFfi {
    pub ono: u32,
    pub class: OcaClassFfi,
    pub role: RString,
    pub settable: bool,
}

impl From<OcaObjectDescriptor> for OcaObjectDescriptorFfi {
    fn from(d: OcaObjectDescriptor) -> Self {
        Self { ono: d.ono.into(), class: d.class.into(), role: d.role.into(), settable: d.settable }
    }
}

impl From<OcaObjectDescriptorFfi> for OcaObjectDescriptor {
    fn from(d: OcaObjectDescriptorFfi) -> Self {
        Self { ono: Ono(d.ono), class: d.class.into(), role: d.role.into(), settable: d.settable }
    }
}

/// FFI-safe mirror of [`dante_babelbox_oca::OcaEvent`] - flattened (no
/// nested address/object structs) since it's simplest to derive
/// `StableAbi` on plain data, not the plain crate's own nested shape.
/// `device_id` carries what `OcaAddress` otherwise would.
#[repr(C)]
#[derive(StableAbi, Debug, Clone, PartialEq)]
pub struct OcaEventFfi {
    pub device_id: RString,
    pub ono: u32,
    pub class: OcaClassFfi,
    pub role: RString,
    pub settable: bool,
    pub value: OcaValueFfi,
}

impl OcaEventFfi {
    pub fn from_event(device_id: impl Into<RString>, object: OcaObject) -> Self {
        Self {
            device_id: device_id.into(),
            ono: object.ono.into(),
            class: object.class.into(),
            role: object.role.into(),
            settable: object.settable,
            value: object.value.into(),
        }
    }

    pub fn into_object(self) -> (String, OcaObject) {
        let device_id = self.device_id.into();
        let object = OcaObject {
            ono: Ono(self.ono),
            class: self.class.into(),
            role: self.role.into(),
            settable: self.settable,
            value: self.value.into(),
        };
        (device_id, object)
    }
}

/// The config needed to construct one device instance. `address` is
/// carried as a plain string (rather than `std::net::IpAddr`, which isn't
/// `StableAbi`) - parsed back into an `IpAddr` on whichever side needs it.
#[repr(C)]
#[derive(StableAbi, Debug, Clone)]
pub struct RDeviceConfig {
    pub id: RString,
    pub address: ROption<RString>,
    pub port: ROption<u16>,
    pub channels: ROption<u16>,
}

#[repr(C)]
#[derive(StableAbi, Debug, Clone)]
pub struct RDeviceInfo {
    pub vendor: RString,
    pub model: RString,
    pub address: RString,
}

/// What a plugin declares about itself, before any device is constructed.
#[repr(C)]
#[derive(StableAbi, Debug, Clone)]
pub struct RPluginInfo {
    pub name: RString,
    pub vendor: RString,
    /// The open-set device "kind" ids (e.g. `"osc-x32"`) this plugin can
    /// build an adapter for - replaces the old closed `DeviceKind` enum.
    pub supported_kinds: RVec<RString>,
}

/// One connected device instance. Constructed by a root module's
/// `create_adapter`. Async operations don't cross this boundary cleanly,
/// so this trait is deliberately synchronous/poll-based rather than
/// mirroring `DeviceAdapter`'s `async fn`s: `connect`/`disconnect` block
/// internally (a plugin manages its own runtime/thread if it needs one),
/// and telemetry/state-change events queue plugin-side until the host
/// drains them with `poll_events` instead of pushing through a broadcast
/// channel. `Send + Sync` as supertraits (both, per `sabi_trait`'s
/// requirement that a reborrowable trait object have both or neither) so
/// the resulting `PluginAdapter_TO` can be owned by a dedicated adapter
/// thread on the host side - every concrete plugin adapter must itself be
/// `Send + Sync` (plain owned socket/buffer state, no `Rc`/`RefCell`) to
/// satisfy this.
#[sabi_trait]
pub trait PluginAdapter: Send + Sync {
    fn id(&self) -> RString;
    fn connect(&mut self) -> RResult<(), RString>;
    /// Stops any background work and releases the connection - the same
    /// requirement `dante_babelbox_core::DeviceAdapter::disconnect` has,
    /// carried over so a plugin-backed device can be removed live without
    /// leaking a socket/thread.
    fn disconnect(&mut self) -> RResult<(), RString>;
    fn identify(&mut self) -> RResult<RDeviceInfo, RString>;
    /// The full set of objects this device instance exposes right now -
    /// channel count may depend on the live device, not just its kind.
    fn describe(&self) -> RVec<OcaObjectDescriptorFfi>;
    fn get_object(&mut self, ono: u32) -> RResult<OcaValueFfi, RString>;
    fn set_object(&mut self, ono: u32, value: OcaValueFfi) -> RResult<(), RString>;
    /// Drains and returns every event queued since the last call - never
    /// blocks.
    #[sabi(last_prefix_field)]
    fn poll_events(&mut self) -> RVec<OcaEventFfi>;
}

pub type PluginAdapterBox = PluginAdapter_TO<'static, abi_stable::std_types::RBox<()>>;

/// The root module every plugin `cdylib` exports via `#[export_root_module]`.
/// A `abi_stable` "prefix type": a `#[repr(C)]` struct of function
/// pointers, not a `#[sabi_trait]` - the root module itself is loaded
/// once per plugin file and never needs dynamic dispatch, only the
/// per-device-instance [`PluginAdapter`] does.
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = PluginRootModule_Ref)))]
#[sabi(missing_field(panic))]
pub struct PluginRootModule {
    pub plugin_info: extern "C" fn() -> RPluginInfo,
    #[sabi(last_prefix_field)]
    pub create_adapter: extern "C" fn(RDeviceConfig) -> RResult<PluginAdapterBox, RString>,
}

impl RootModule for PluginRootModule_Ref {
    abi_stable::declare_root_module_statics! {PluginRootModule_Ref}

    const BASE_NAME: &'static str = "dante_babelbox_plugin";
    const NAME: &'static str = "dante_babelbox_plugin";
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_class_round_trips_every_variant() {
        let classes = [
            OcaClass::Gain,
            OcaClass::Mute,
            OcaClass::Switch,
            OcaClass::Polarity,
            OcaClass::Delay,
            OcaClass::BasicSensor,
            OcaClass::LevelSensor,
            OcaClass::AudioLevelSensor,
            OcaClass::BooleanSensor,
            OcaClass::Int32Sensor,
            OcaClass::StringSensor,
        ];
        for class in classes {
            let ffi: OcaClassFfi = class.into();
            let back: OcaClass = ffi.into();
            assert_eq!(class, back);
        }
    }

    #[test]
    fn ffi_value_round_trips_every_variant() {
        for value in [
            OcaValue::F32(-6.0),
            OcaValue::I32(80),
            OcaValue::Bool(true),
            OcaValue::String("Inactive".into()),
        ] {
            let ffi: OcaValueFfi = value.clone().into();
            let back: OcaValue = ffi.into();
            assert_eq!(value, back);
        }
    }

    #[test]
    fn ffi_descriptor_round_trips() {
        let descriptor = OcaObjectDescriptor {
            ono: Ono(42),
            class: OcaClass::Gain,
            role: "Ch 1 Gain".into(),
            settable: true,
        };
        let ffi: OcaObjectDescriptorFfi = descriptor.clone().into();
        let back: OcaObjectDescriptor = ffi.into();
        assert_eq!(descriptor, back);
    }

    #[test]
    fn ffi_event_round_trips_through_object_and_device_id() {
        let object = OcaObject {
            ono: Ono(7),
            class: OcaClass::AudioLevelSensor,
            role: "Audio Level".into(),
            settable: false,
            value: OcaValue::F32(-20.0),
        };
        let ffi = OcaEventFfi::from_event("shure-1", object.clone());
        let (device_id, back) = ffi.into_object();
        assert_eq!(device_id, "shure-1");
        assert_eq!(back, object);
    }
}

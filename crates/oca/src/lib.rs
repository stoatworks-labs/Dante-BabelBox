//! An in-process object model inspired by AES70/OCA's vocabulary and
//! addressing shape, used as this bridge's one internal representation for
//! every device family (preamp control, radio-mic telemetry, and whatever
//! comes after). Replaces the old per-domain `PreampState`/`PreampEvent`
//! and `MicState`/`MicEvent` types.
//!
//! **This is not a wire protocol.** There is no OCP.1 traffic here, no
//! socket, and no dependency on (or code shared with) the user's separate
//! `~/Projects/db-remote` project, whose `aes70` crate *is* a real,
//! tested OCP.1 wire codec for an unrelated project that may evolve
//! independently. That project's `crates/aes70::classes` module (a
//! vendor-neutral AES70-1 class-hierarchy reference) is what grounded the
//! choice to reuse AES70's class taxonomy here at all - it confirmed
//! `OcaGain`/`OcaMute`/`OcaSwitch`/`OcaLevelSensor`/`OcaAudioLevelSensor`
//! etc. are a genuine, standard-defined fit for both this project's
//! existing domains, not an invented shoehorn. Nothing outside this
//! process decodes an [`Ono`] yet; a real OCP.1 listener over this model
//! is a plausible future extension, not something this crate does today.
//!
//! Deliberate, documented modeling compromises (see also `object.rs`'s
//! tests):
//! - Preamp phantom power and pad are modeled as [`OcaClass::Switch`]
//!   (a 2-position switch), not a dedicated boolean-actuator class - the
//!   public sources this project trusts don't consistently document one.
//! - Mic-telemetry antenna diversity (`A`/`B`/`Inactive`) is modeled as
//!   [`OcaClass::StringSensor`] with a string value, not a bespoke 3-state
//!   enum-sensor class.
//! - `frequency_mhz` (`f64` in the old `MicState`) is carried as
//!   [`OcaValue::F32`], trading away sub-Hz precision on an already
//!   MHz-scale value.

mod object;
mod ono;
mod value;

pub use object::{OcaAddress, OcaEvent, OcaObject, OcaObjectDescriptor};
pub use ono::{Ono, OnoAllocator};
pub use value::{OcaClass, OcaValue};

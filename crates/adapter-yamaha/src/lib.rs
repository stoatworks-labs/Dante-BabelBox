//! `DeviceAdapter` implementations for Yamaha gear.
//!
//! [`dm3::Dm3Adapter`] covers DM3/DM3S via the official "DM3 Series OSC
//! Specifications V1.0.0" - the only Yamaha console line in this project
//! with a byte/field-level public spec.
//!
//! CL/QL/DM7 and Rio/Tio HA control remain unimplemented: Yamaha's own
//! "Dante-MY16-AUD & R series HA Remote Control Guide" confirms these use
//! the legacy AD8HR MIDI protocol bridged over serial or the Dante
//! network, but that guide only covers system setup (DIP switches, Dante
//! Controller bridging modes), not the actual wire bytes - would need
//! packet captures of a real console talking to a Rio/Tio, or a copy of
//! the AD8HR/DME MIDI SysEx spec, to implement correctly rather than
//! guessing.

mod dm3;

pub use dm3::Dm3Adapter;

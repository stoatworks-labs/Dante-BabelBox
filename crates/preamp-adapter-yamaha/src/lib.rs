//! `DeviceAdapter` implementations for Yamaha gear.
//!
//! [`dm3::Dm3Adapter`] covers DM3/DM3S via the official "DM3 Series OSC
//! Specifications V1.0.0" - the only Yamaha console line in this project
//! with a byte/field-level public spec.
//!
//! CL/QL/DM7 and Rio/Tio HA control remain **unimplemented, but no longer
//! unknown** - the wire format has been captured from real hardware and is
//! documented in `docs/yamaha-ha-remote-over-dante.md`. That document is
//! the spec to implement against; it is not guesswork.
//!
//! CORRECTION to what this file previously claimed: R-series HA control is
//! *not* the legacy AD8HR MIDI protocol. A native QL1 <-> Rio3224-D2
//! pairing carries gain and phantom in a Yamaha-proprietary binary block
//! marked `MBC`, tunnelled inside Audinate ConMon vendor messages on the
//! Dante control ports - there is no MIDI SysEx anywhere in it (no F0/F7
//! framing, no SysEx structure). Yamaha's "Dante-MY16-AUD & R series HA
//! Remote Control Guide" describes the *MY16 card bridging* case, which is
//! a different path; do not go looking for the AD8HR SysEx spec to
//! implement a console with built-in Dante.
//!
//! What the capture work established, against real gear:
//!   - gain: 32 x int16 big-endian, **centi-dB** (`-600` == -6.00 dB),
//!     range -6.00..+66.00 on a Rio3224-D2 - divide by 100 for
//!     `PreampState::gain_db`.
//!   - phantom: 32 x uint8 boolean.
//!   - metering: 32 x uint8 at ~31 Hz.
//!   - the trailing checksum, the ConMon envelope, and the fact that a
//!     hand-built gain message **is accepted and acted on by a real
//!     Rio3224-D2**.
//!
//! Not yet resolved, so don't invent them: `pad`, HPF, polarity and
//! digital trim each correspond to one of eight 32-element arrays whose
//! element width and meaning were never observed populated.

mod dm3;

pub use dm3::Dm3Adapter;

//! `DeviceAdapter` implementations for Allen & Heath gear.
//!
//! [`ahm::AhmAdapter`] covers AHM-series Dante/AES67 processors via the
//! official AHM TCP/IP Protocol V1.4 spec (NRPN-over-TCP, port 51325).
//!
//! [`dlive::DliveAdapter`] covers dLive consoles/MixRacks via the official
//! dLive MIDI Over TCP/IP Protocol V2.0 spec, which - unlike SQ/Qu -
//! explicitly documents preamp gain/pad/phantom control via physical
//! "Socket" addressing distinct from processing channels.
//!
//! Qu/SQ preamp control remains unimplemented: their public MIDI protocol
//! spec doesn't document preamp control at all. See `ahm.rs`'s module doc
//! comment for details.

mod ahm;
mod dlive;

pub use ahm::AhmAdapter;
pub use dlive::DliveAdapter;

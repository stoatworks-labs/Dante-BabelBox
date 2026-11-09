//! A virtual Dante device's control plane.
//!
//! This crate puts a device on a Dante network that is not there: it
//! advertises it over mDNS, answers Audinate's ARC queries about its name
//! and channels, takes ConMon registrations and publishes the status
//! messages a real device publishes. It carries **no audio**, runs no PTP
//! clock, and opens no flows.
//!
//! What it is for: a console that controls a stagebox's head amps does so
//! by talking to the stagebox as a Dante device. Present the console with a
//! device it knows, and its own gain and +48 V controls arrive here, where
//! the bridge can send them on to hardware the console has never heard of.
//! The vendor's control protocol is not in this crate. It rides on the
//! vendor channel ([`VirtualDevice::vendor`]) and lives with the vendor's
//! adapter, the way every other vendor specific in this workspace does.
//!
//! ## Layers
//!
//! | Module | What it answers | Where |
//! |---|---|---|
//! | [`mdns`] | "which Dante devices are out there?" | mDNS, UDP 5353 |
//! | [`arc`] | "what are you, what channels have you got?" | UDP 4440 |
//! | [`cmc`] | "send me your status channel" | UDP 8800 |
//! | [`status`] | "what is your clock / sample rate / make and model?" | UDP 8700 in, 8702 out |
//! | [`VendorChannel`] | the vendor's own messages | 224.0.0.232:8705 |
//!
//! The first four are pure functions over bytes with the sockets kept in
//! [`device`], so each is tested against frames from a real device.
//!
//! ## Evidence
//!
//! Everything here was read off one packet capture: a Yamaha QL1 pairing
//! with a Rio3224-D2. `docs/dante-emulation.md` says what that does and
//! does not establish. In short: the replies are the Rio's replies, byte for
//! byte, and **no console has yet been put in front of this code.**

pub mod arc;
pub mod capture;
pub mod cmc;
pub mod conmon;
pub mod device;
mod hexutil;
pub mod identity;
pub mod mdns;
pub mod persona;
pub mod status;

pub use device::{EmulationError, Ports, VendorChannel, VendorDatagram, VirtualDevice, VirtualDeviceConfig};
pub use identity::{local_mac, parse_mac, validate_name, DeviceIdentity, IdentityError, MacDisplay};
pub use persona::Persona;

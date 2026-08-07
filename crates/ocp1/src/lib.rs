//! A vendor-neutral AES70 (OCA) controller.
//!
//! AES70-1 defines the object model — every controllable parameter is an
//! *object* with a 32-bit **object number (ONo)**, belonging to a class such as
//! `OcaGain` or `OcaSwitch`. AES70-3 (**OCP.1**) defines how to talk to those
//! objects over TCP. This crate implements the controller side of both.
//!
//! ```no_run
//! # async fn example() -> Result<(), dante_babelbox_ocp1::Error> {
//! use dante_babelbox_ocp1::{classes::level, client::Client, enumerate};
//!
//! let client = Client::connect(("192.168.1.100", 65000)).await?;
//! for object in enumerate::enumerate(&client).await? {
//!     println!("{} {} {:?}", object.ono, object.qualified_role(), object.class.name());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Scope, and what is not verified
//!
//! There is no per-vendor ONo map here, deliberately: [`enumerate`] walks the
//! device's own block tree so a device describes itself. What that costs is a
//! dependency on two things this project has not been able to check against
//! real hardware — the `OcaBlock::GetMembers` method index and the marshalling
//! of its reply. Both are handled by trying alternatives rather than assuming;
//! see [`enumerate`]'s module comment for exactly how.
//!
//! **No part of this crate has been run against an AES70 device.** Its tests
//! run against a mock that answers the way this crate expects a device to,
//! which proves the two halves agree with each other and nothing more.
//!
//! ## Provenance
//!
//! The OCP.1 framing in [`pdu`] and the marshalling in [`value`] are ported
//! from this author's `db-remote` project, where they were derived from the
//! AES70 standard, from d&b audiotechnik's published integration material, and
//! by cross-checking wire offsets against the GPLv3 NanoOcp project. Facts
//! about a wire protocol are not themselves copyrightable and no code was
//! copied from NanoOcp — both implementations are independent and MIT-licensed.
//! The d&b-specific ONo bit-packing that sat alongside them there is
//! deliberately *not* carried over: it is one vendor's private layout, and this
//! crate has no business knowing it.

pub mod classes;
pub mod client;
pub mod enumerate;
pub mod ono;
pub mod pdu;
pub mod value;

pub use client::Client;
pub use enumerate::DiscoveredObject;
pub use ono::Ono;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection closed")]
    Disconnected,
    #[error("timed out waiting for a response")]
    Timeout,
    #[error("truncated message")]
    Truncated,
    #[error("bad sync byte 0x{0:02x}")]
    BadSync(u8),
    #[error("unsupported protocol version {0}")]
    BadVersion(u16),
    #[error("implausible pdu length {0}")]
    BadLength(usize),
    #[error("unknown pdu type {0}")]
    UnknownPduType(u8),
    #[error("device returned {name} ({code})")]
    Status { code: u8, name: &'static str },
}

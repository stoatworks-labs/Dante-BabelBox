//! `MicAdapter` implementation for Lectrosonics networked receivers
//! (DSQD / D Squared, Duet, DCR822).
//!
//! ⚠️ UNLIKE the Shure and Sennheiser adapters in this workspace, this one is
//! **NOT** built from an authoritative Lectrosonics protocol document. Its
//! Ethernet control port is real and documented as open to Wireless Designer
//! and third-party control, but the exact wire format here is a **placeholder**
//! mirroring the RFutils adapter of the same name. Every field mapping is
//! provisional pending the official IP-control spec or a packet capture. See
//! [`lectrosonics::LectrosonicsAdapter`]'s module doc comment for the full
//! scope note, and treat this crate as "framing scaffold, field-level behavior
//! unverified" until confirmed.

mod lectrosonics;

pub use lectrosonics::{LectrosonicsAdapter, DEFAULT_LECTROSONICS_PORT};

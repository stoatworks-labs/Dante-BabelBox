//! `MicAdapter` implementation for Sennheiser gear.
//!
//! [`sennheiser::SennheiserAdapter`] covers EW-DX EM 2 / EM 2 Dante / EM 4
//! Dante receivers via Sennheiser's official Sound Control Protocol (SSC)
//! over UDP - see its module doc comment for exact spec sources and scope
//! notes.

mod sennheiser;

pub use sennheiser::SennheiserAdapter;

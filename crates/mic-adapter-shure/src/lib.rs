//! `MicAdapter` implementation for Shure gear.
//!
//! [`shure::ShureAdapter`] covers ULX-D and Axient Digital receivers via
//! Shure's official ASCII command-string protocol (TCP port 2202) - see
//! its module doc comment for exact spec sources and scope notes.

mod shure;

pub use shure::ShureAdapter;

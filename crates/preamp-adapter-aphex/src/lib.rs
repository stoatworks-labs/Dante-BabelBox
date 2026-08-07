//! Aphex 1788A parametric MIDI control.
//!
//! **Protocol source:** Aphex's published "1788A Parametric MIDI Commands"
//! page — the command table and the System Exclusive control-string layout.
//! Every opcode, value range and frame offset in [`codec`] comes from that one
//! document; nothing is inferred from captures or from another vendor's
//! behaviour.
//!
//! The 1788A is an 8-channel remote-controlled mic preamp. It is worth having
//! here because it exercises more of this project's model than anything else it
//! supports: gain, phantom, pad, polarity, mute and a low-cut filter are all
//! addressable, where most vendors expose only the first two.
//!
//! ## Codec only — there is no transport yet
//!
//! This crate builds and parses the byte string. It does not deliver it.
//! That is a deliberate stopping point rather than an unfinished one, because
//! the delivery path is a genuine open question and the codec is the same
//! either way:
//!
//! - The published table is the **MIDI** layer, so one route is a physical MIDI
//!   port. That would make this the only device here not reached over an IP
//!   socket, and would pull a MIDI backend (and, on Linux, ALSA headers in
//!   release CI) into a workspace that currently needs neither.
//! - The 1788A also has an **Ethernet** control port, and Aphex sold a Model
//!   5200 MIDI/RS-422-to-LAN interface aimed at Avid and Yamaha consoles. If
//!   that path carries these same SysEx bytes, the adapter is an ordinary
//!   socket adapter and this crate is already most of it. **How SysEx is framed
//!   over the network port is not documented on the page this was built from**,
//!   so that is not assumed either way.
//!
//! Until one of those is settled, no `DeviceAdapter` is implemented and no
//! plugin ships. What exists is exhaustively tested against the document.
//!
//! ## What is not known from the source document
//!
//! Flagged here rather than papered over, because each is a question a caller
//! will hit immediately:
//!
//! - **Whether "Mic Channel" counts from 0 or from 1**, and whether any value
//!   addresses all channels at once. [`codec::Command`] therefore carries the
//!   raw byte and performs no arithmetic on it.
//! - **What the `20h`/`56h` dump replies look like.** The table documents the
//!   requests, not the responses. [`codec::Message::decode`] assumes a reply
//!   reuses the control-string shape, which is the natural reading but is
//!   unconfirmed.
//! - **What `MIDI Channel`, `MIDI Device` and `Net Number` each select**, and
//!   how they map to the unit's front-panel settings.
//!
//! None of these blocks the codec; all of them want ten minutes with a unit.

pub mod codec;

pub use codec::{Command, Control, DeviceAddress, Message};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{field} is {value:#04x}, which is not a MIDI data byte (must be <= 0x7f)")]
    NotADataByte { field: &'static str, value: u8 },
    #[error("{control:?} takes {min:#04x}..={max:#04x}, got {value:#04x}")]
    ValueOutOfRange { control: Control, value: u8, min: u8, max: u8 },
    #[error("{0:?} is not an on/off control")]
    NotASwitch(Control),
    #[error("a message needs at least one command")]
    NoCommands,
    #[error("{0} commands, but a message carries at most 64")]
    TooManyCommands(usize),
    #[error("too short to be a parametric control string")]
    Truncated,
    #[error("does not start with F0 (got {0:#04x})")]
    NotSysEx(u8),
    #[error("no F7 terminator")]
    Unterminated,
    #[error("manufacturer id {0:02x?} is not Aphex")]
    ForeignManufacturer([u8; 3]),
    #[error("{0} body bytes is not a whole number of 3-byte commands")]
    RaggedBody(usize),
    #[error("opcode {0:#04x} is not on the published command table")]
    UnknownOpcode(u8),
}

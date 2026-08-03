//! Shure QLX-D telemetry over **ACN**, the control path a receiver uses
//! when it is mounted on a Yamaha console.
//!
//! This is a second, entirely separate protocol from the one
//! `mic-adapter-shure` speaks. That adapter uses Shure's documented
//! Command Strings (plaintext ASCII, TCP 2202); a QLX-D mounted on a
//! console uses ANSI E1.17 ACN on UDP instead, and no capture of a
//! console-mounted receiver contains a single byte of port-2202 traffic.
//! Neither adapter replaces the other.
//!
//! Built against `docs/SHURE-ACN.md` in RFutils, reverse-engineered from
//! captures of a real QLXD4 mounted on a real Yamaha QL1 (archived in the
//! private `dante-captures` repo). The decoder in [`acn`] is verified
//! against real receiver frames; [`slp`] against the receiver's real
//! advertisement.
//!
//! # What this adapter can and cannot do
//!
//! **Discovery works anywhere on the segment.** Receivers multicast an
//! SLPv2 advertisement every ~2 s carrying model, user-assigned name, CID
//! and session endpoint. Listening for it is passive and reliable.
//!
//! **Telemetry is passive, and needs the traffic to be visible.** After
//! session setup the receiver pushes `EVENT` messages at ~8 Hz - but it
//! sends them **unicast to the console**. On an ordinary switch port they
//! never reach this host. Reading telemetry therefore requires a mirrored
//! port, a hub, or running on the console itself. The adapter reports what
//! it can see and does not pretend a silent receiver is absent.
//!
//! **It cannot open its own session.** An active DMP client would let this
//! subscribe for events directly rather than eavesdropping, and the spec
//! describes the JOIN / CONNECT / SUBSCRIBE sequence - but only as a
//! sequence. The capture that contained the byte-level handshake is
//! **lost** (see the `dante-captures` README), so the field layouts cannot
//! be verified against evidence. Rather than ship a guess that fails
//! silently, session establishment is deliberately not implemented. It
//! needs one fresh capture of a receiver power-cycling while mounted.
//!
//! # What is deliberately not reported
//!
//! - **Audio level.** `0x02000812` was the obvious candidate and does not
//!   hold up: it keeps moving with no carrier at all, which no audio meter
//!   on a muted receiver would do. It is decoded, given no unit, and never
//!   mapped to `audio_level_dbfs`.
//! - **Battery percentage.** The wire carries 0-5 bars matching the
//!   receiver's five-segment display. That goes to `battery_bars`;
//!   multiplying it into a percentage would invent precision.
//! - **Battery run time.** Subscribed but never once emitted across every
//!   capture, including a full battery swap. Probably needs an SB900-series
//!   rechargeable fitted; unconfirmed, so unreported.
//! - **Mute.** No mute property was identified in the property map, so
//!   `muted` is always `false` here rather than a real reading, and
//!   `set_mute` returns an error instead of silently doing nothing.

pub mod acn;
pub mod adapter;
pub mod slp;

pub use adapter::ShureAcnAdapter;
pub use slp::Advertisement;

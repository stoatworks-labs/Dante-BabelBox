//! RME **TotalMix FX** output faders, over TotalMix's Global OSC.
//!
//! Exposes the hardware outputs of an RME interface, such as a Digiface
//! Dante, as settable gains, so another device's control can drive an
//! output level through the bridge. Channel N is TotalMix output N; its
//! gain is that output's fader in dB, -65 (off) to +6. TotalMix runs on the
//! computer the interface is plugged into, so `address` is that computer
//! (usually 127.0.0.1), not the interface.
//!
//! **Source.** The addresses, arguments and fader law come from the
//! `rme-totalmix` Companion module 1.0.1 (MIT), whose Global OSC mode needs
//! TotalMix FX 2.1 or later. That module's Global OSC routing actions ran
//! against a real Digiface Dante with TotalMix FX 2.10 on 2026-11-05, for
//! the SuperRack failsafe page. Its output-fader message,
//! `/output/<index>/faderlin`, has not been run on hardware by this project:
//! this adapter is tested against a stand-in only.
//!
//! **TotalMix setup.** In TotalMix's OSC settings, turn on OSC in Global
//! mode, set Remote IP to this machine and leave the ports at 7001 in and
//! 9001 out. TotalMix replies to one remote only, so nothing else may be on
//! UDP 9001 here (Companion's TotalMix module listens there by default).
//!
//! **Levels are 1:1 in dB.** The bridge's mappings copy dB unchanged, and a
//! preamp's gain sits far above an output fader's range: a +30 dB preamp
//! mapped straight onto an output puts it at +6 dB, TotalMix's top. Don't
//! map a preamp onto an output until something offsets the level.

pub mod adapter;
pub mod law;

pub use adapter::{TotalMixAdapter, REPLY_PORT, TOTALMIX_PORT};

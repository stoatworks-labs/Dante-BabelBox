//! Sennheiser evolution-wireless **G3** rack-receiver control.
//!
//! Exposes an EM 300/500 G3's analog **AF-out level** as a settable gain,
//! so a console's remote head-amp gain (e.g. a Yamaha DM3 over this bridge)
//! can drive it. Speaks the receiver's binary WSM protocol on UDP 8133 —
//! see [`codec`] for the wire details and provenance.
//!
//! The G3 has no remote phantom power and a single audio output, so the
//! adapter presents exactly one channel whose "gain" is the AF-out level
//! (`-24..=+18` dB in 3 dB steps). `set_phantom` is accepted and ignored.
//!
//! ## Single-master write lock
//!
//! The receiver honours writes only from the one client holding its master
//! slot. This adapter keeps that slot by sending a keepalive every 2 s while
//! connected. **Wireless Systems Manager, if running and pointed at the same
//! receiver, will hold the slot instead and this adapter's writes will be
//! silently dropped** — close WSM before bridging.

pub mod adapter;
pub mod codec;

pub use adapter::Ewg3Adapter;

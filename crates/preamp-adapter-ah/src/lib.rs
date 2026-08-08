//! `DeviceAdapter` implementations and codecs for Allen & Heath gear.
//!
//! [`ahm::AhmAdapter`] covers AHM-series Dante/AES67 processors via the
//! official AHM TCP/IP Protocol V1.4 spec (NRPN-over-TCP, port 51325).
//!
//! [`dlive::DliveAdapter`] covers dLive consoles/MixRacks via the official
//! dLive MIDI Over TCP/IP Protocol V2.0 spec, which - unlike SQ/Qu -
//! explicitly documents preamp gain/pad/phantom control via physical
//! "Socket" addressing distinct from processing channels.
//!
//! [`dt`] is a **codec** for the DT168 / DT164-W Dante expanders, recovered by
//! reverse-engineering A&H's *DT Preamp Control* app and the SQ Dante-card
//! firmware. These stageboxes are **not** controlled over the console MIDI
//! protocol at all - they answer Audinate **ConMon vendor messages** (vendor
//! ID `AllenHth`). See `docs/allenheath-dt-preamp-over-dante.md`. There is no
//! transport yet (ConMon needs the Audinate DAPI, which this workspace does not
//! link), so `dt` builds/parses the vendor payload only - the same stopping
//! point as `preamp-adapter-aphex`.
//!
//! ## Qu / SQ status
//!
//! Qu/SQ preamp control has no adapter here, but the DT work changed the
//! picture. The SQ's own Dante card is A&H's **KLANTE** module, and it
//! registers the same **`AllenHth`** ConMon namespace the DT expanders use - so
//! [`dt`] is the most promising SQ-over-Dante preamp path, pending a capture
//! that confirms an SQ answers the mic-pre message set and how it addresses its
//! sockets. The console MIDI-over-TCP route is a weaker fallback: the published
//! SQ MIDI protocol documents no preamp messages, so it is left as a documented
//! hypothesis (reuse the AHM/dLive NRPN scheme) rather than fabricated code.
//! See `ahm.rs`'s module doc comment and the DT doc for the full reasoning.

mod ahm;
mod dlive;
pub mod dt;

pub use ahm::AhmAdapter;
pub use dlive::DliveAdapter;

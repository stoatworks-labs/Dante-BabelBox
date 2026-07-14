//! `DeviceAdapter` implementations for OSC-controlled desks.
//!
//! [`x32::X32Adapter`] covers the X32-family dialect shared by Behringer
//! X32 and Midas M32/HD96. [`wing::WingAdapter`] covers Behringer Wing's
//! related but distinct OSC dialect on port 2223 - see its module doc
//! comment for scope limits (currently only the console's 8 built-in LCL
//! preamps, not AES50/StageConnect-attached stageboxes).

mod wing;
mod x32;

pub use wing::WingAdapter;
pub use x32::X32Adapter;

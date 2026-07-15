//! Default ports per protocol, shared between `daemon::run` (which needs
//! them when a `bridge.toml` entry omits `port`) and `init::run` (which
//! needs them to know what to probe).

pub(crate) const DEFAULT_X32_PORT: u16 = 10023;
pub(crate) const DEFAULT_AHM_PORT: u16 = 51325;
pub(crate) const DEFAULT_WING_PORT: u16 = 2223;
pub(crate) const DEFAULT_DLIVE_PORT: u16 = 51325;
pub(crate) const DEFAULT_DM3_PORT: u16 = 49900;

//! Object numbers (ONos).
//!
//! AES70 itself treats an ONo as an opaque `u32` and reserves a handful of
//! well-known values for the device's managers. Everything above those is the
//! vendor's to structure however it likes — which is exactly why this crate
//! *enumerates* a device's objects (see [`crate::enumerate`]) rather than
//! carrying a per-vendor ONo layout.

/// A 32-bit AES70 object number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ono(pub u32);

impl std::fmt::Display for Ono {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for Ono {
    fn from(v: u32) -> Self {
        Ono(v)
    }
}

impl From<Ono> for u32 {
    fn from(v: Ono) -> Self {
        v.0
    }
}

impl Ono {
    /// Reserved ONos are the device's managers and its root block; everything
    /// above is vendor-allocated.
    pub const fn is_reserved(self) -> bool {
        self.0 <= reserved::ROOT_BLOCK.0
    }
}

/// ONos reserved by AES70-1 for every compliant device.
pub mod reserved {
    use super::Ono;

    pub const DEVICE_MANAGER: Ono = Ono(1);
    pub const SECURITY_MANAGER: Ono = Ono(2);
    pub const FIRMWARE_MANAGER: Ono = Ono(3);
    pub const SUBSCRIPTION_MANAGER: Ono = Ono(4);
    pub const POWER_MANAGER: Ono = Ono(5);
    pub const NETWORK_MANAGER: Ono = Ono(6);
    pub const MEDIA_CLOCK_MANAGER: Ono = Ono(7);
    pub const LIBRARY_MANAGER: Ono = Ono(8);
    pub const AUDIO_PROCESSING_MANAGER: Ono = Ono(9);
    pub const DEVICE_TIME_MANAGER: Ono = Ono(10);
    pub const TASK_MANAGER: Ono = Ono(11);
    pub const CODING_MANAGER: Ono = Ono(12);
    pub const DIAGNOSTIC_MANAGER: Ono = Ono(13);
    /// The device's top-level `OcaBlock`, the root of the object tree.
    pub const ROOT_BLOCK: Ono = Ono(100);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managers_and_the_root_block_are_reserved() {
        assert!(reserved::DEVICE_MANAGER.is_reserved());
        assert!(reserved::ROOT_BLOCK.is_reserved());
        assert!(!Ono(101).is_reserved());
        assert!(!Ono(0x1000_8206).is_reserved());
    }

    #[test]
    fn onos_display_as_padded_hex() {
        assert_eq!(Ono(0x0100_8206).to_string(), "0x01008206");
    }
}

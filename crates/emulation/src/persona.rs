//! What a virtual device claims to be at the Dante level: maker, model and
//! channel counts.
//!
//! A persona holds no vendor control protocol. A console's head-amp protocol
//! (Yamaha's `MBC`, for one) is a separate layer that rides on the vendor
//! channel; see `VirtualDevice::vendor`.

/// The Dante-level description of a device.
///
/// Every field that reaches the wire is written where a real device of that
/// make writes it: mDNS TXT records, the ARC device-info reply and the
/// ConMon make-and-model status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    /// Short maker id, at most 8 bytes: mDNS `mf=`, the `<maker>._sub`
    /// service subtype, and the make-and-model status. `Yamaha`.
    pub manufacturer_id: String,
    /// Maker's full name. `Yamaha Corporation`.
    pub manufacturer_name: String,
    /// Audinate model id, at most 8 bytes: mDNS `model=`. `8R19` for a
    /// Rio3224-D2.
    pub model_id: String,
    /// Product name as the maker prints it. `Rio3224-D2`.
    pub model_name: String,
    /// Product firmware version, `(major, minor)`: `(1, 85)` is `V1.85`.
    pub product_version: (u8, u8),
    /// Channels the device sends to the network. A stagebox's inputs.
    pub tx_channels: u16,
    /// Channels the device receives from the network. A stagebox's outputs.
    pub rx_channels: u16,
    pub sample_rate: u32,
    /// Prefix of the Dante module's own host name, which is what the mDNS
    /// SRV records point at (`BROOKLYN3-25df04.local`).
    pub module_host_prefix: String,
}

impl Persona {
    /// A Yamaha Rio3224-D2, as captured: 32 inputs, 24 outputs, Brooklyn 3.
    ///
    /// This is the only persona whose every value was read off a real
    /// device. See `docs/dante-emulation.md`.
    pub fn yamaha_rio3224_d2() -> Self {
        Self {
            manufacturer_id: "Yamaha".into(),
            manufacturer_name: "Yamaha Corporation".into(),
            model_id: "8R19".into(),
            model_name: "Rio3224-D2".into(),
            product_version: (1, 85),
            tx_channels: 32,
            rx_channels: 24,
            sample_rate: 48_000,
            module_host_prefix: "BROOKLYN3".into(),
        }
    }

    /// Name of transmit channel `n` (1-based). Dante's default is the
    /// zero-padded number, which is what the captured Rio advertised.
    pub fn tx_channel_name(&self, n: u16) -> String {
        format!("{n:02}")
    }

    /// Name of receive channel `n` (1-based).
    pub fn rx_channel_name(&self, n: u16) -> String {
        format!("{n:02}")
    }

    /// `V1.85`
    pub fn product_version_text(&self) -> String {
        format!("V{}.{:02}", self.product_version.0, self.product_version.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rio_persona_matches_what_the_capture_showed() {
        let p = Persona::yamaha_rio3224_d2();
        assert_eq!(p.tx_channel_name(1), "01");
        assert_eq!(p.tx_channel_name(32), "32");
        assert_eq!(p.product_version_text(), "V1.85");
        assert!(p.manufacturer_id.len() <= 8 && p.model_id.len() <= 8);
    }
}

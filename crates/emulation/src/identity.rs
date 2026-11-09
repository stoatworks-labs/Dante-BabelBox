//! Who a virtual device says it is: a Dante device name, a MAC and the
//! address it is presented on.

use std::fmt;
use std::net::Ipv4Addr;

/// The identity a virtual device presents.
///
/// `mac` is the device's *claimed* hardware address. Nothing here changes
/// the address frames leave the host with: every packet is ordinary UDP from
/// the host's own interface. The claimed MAC only appears inside payloads,
/// where Dante uses it as the device's permanent id (the ConMon sender id is
/// the MAC with `ff:fe` in the middle, and a Yamaha head-amp block is
/// addressed by it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Dante device name, e.g. `Y005-Yamaha-Rio3224-D2-a1b2c3`.
    pub name: String,
    pub mac: [u8; 6],
    /// The local interface address the device is presented on.
    pub ip: Ipv4Addr,
}

impl DeviceIdentity {
    /// The 8-byte ConMon device id: the MAC with `ff:fe` inserted, which is
    /// what a Brooklyn-based device sends (`00:1d:c1:25:df:04` goes out as
    /// `001dc1fffe25df04`).
    pub fn eui64(&self) -> [u8; 8] {
        eui64(self.mac)
    }

    /// The last three MAC bytes in lowercase hex, the suffix Dante puts on a
    /// default device name and on the module's host name.
    pub fn mac_suffix(&self) -> String {
        mac_suffix(self.mac)
    }
}

pub fn eui64(mac: [u8; 6]) -> [u8; 8] {
    [mac[0], mac[1], mac[2], 0xff, 0xfe, mac[3], mac[4], mac[5]]
}

pub fn mac_suffix(mac: [u8; 6]) -> String {
    format!("{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("'{0}' is not a MAC address (expected six hex bytes, e.g. 02:bb:5a:01:02:03)")]
    BadMac(String),
    #[error("device name '{0}' is not a valid Dante name: {1}")]
    BadName(String, &'static str),
}

/// Parses `02:bb:5a:01:02:03`, `02-bb-5a-01-02-03` or `02bb5a010203`.
pub fn parse_mac(text: &str) -> Result<[u8; 6], IdentityError> {
    let digits: String = text.chars().filter(|c| !matches!(c, ':' | '-' | '.')).collect();
    if digits.len() != 12 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(IdentityError::BadMac(text.to_string()));
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16)
            .map_err(|_| IdentityError::BadMac(text.to_string()))?;
    }
    Ok(mac)
}

/// A stable, locally administered MAC for a virtual device, derived from
/// `seed` (the bridge uses the device id plus the address it is presented
/// on).
///
/// The first byte is `02`: the locally-administered bit is set and the
/// multicast bit is clear, so the address can never collide with a real
/// manufacturer's block. That matters because the address is an identity on
/// a network that also carries real devices.
pub fn local_mac(seed: &str) -> [u8; 6] {
    // FNV-1a, 64 bit. Not cryptographic and does not need to be: it only has
    // to give the same five bytes for the same seed on every run.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let h = hash.to_be_bytes();
    [0x02, h[3], h[4], h[5], h[6], h[7]]
}

/// Dante device names: 1 to 31 characters, letters, digits and hyphens, not
/// starting or ending with a hyphen. (Dante Controller enforces the same
/// rule when a device is renamed.)
pub fn validate_name(name: &str) -> Result<(), IdentityError> {
    let bad = |why| Err(IdentityError::BadName(name.to_string(), why));
    if name.is_empty() {
        return bad("it is empty");
    }
    if name.len() > 31 {
        return bad("it is longer than 31 characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return bad("only letters, digits and hyphens are allowed");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return bad("it cannot start or end with a hyphen");
    }
    Ok(())
}

/// `00:1d:c1:25:df:04`
pub struct MacDisplay(pub [u8; 6]);

impl fmt::Display for MacDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eui64_inserts_fffe_like_the_captured_rio() {
        // Rio3224-D2 in the reference capture: Dante MAC 00:1d:c1:25:df:04,
        // ConMon sender id 001dc1fffe25df04.
        assert_eq!(
            eui64([0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04]),
            [0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x25, 0xdf, 0x04]
        );
        assert_eq!(mac_suffix([0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04]), "25df04");
    }

    #[test]
    fn parses_the_usual_mac_spellings() {
        let want = [0x02, 0xbb, 0x5a, 0x01, 0x02, 0x03];
        assert_eq!(parse_mac("02:bb:5a:01:02:03").unwrap(), want);
        assert_eq!(parse_mac("02-BB-5A-01-02-03").unwrap(), want);
        assert_eq!(parse_mac("02bb5a010203").unwrap(), want);
        assert!(parse_mac("02:bb:5a:01:02").is_err());
        assert!(parse_mac("not a mac").is_err());
    }

    #[test]
    fn a_derived_mac_is_locally_administered_unicast_and_stable() {
        let a = local_mac("virtual-rio@169.254.10.20");
        assert_eq!(a[0] & 0x02, 0x02, "locally administered bit set");
        assert_eq!(a[0] & 0x01, 0x00, "multicast bit clear");
        assert_eq!(a, local_mac("virtual-rio@169.254.10.20"));
        assert_ne!(a, local_mac("virtual-rio@169.254.10.21"));
    }

    #[test]
    fn device_names_follow_dantes_rule() {
        assert!(validate_name("Y004-Yamaha-Rio3224-D2-25df04").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("-leading").is_err());
        assert!(validate_name(&"x".repeat(32)).is_err());
        assert!(validate_name(&"x".repeat(31)).is_ok());
    }
}

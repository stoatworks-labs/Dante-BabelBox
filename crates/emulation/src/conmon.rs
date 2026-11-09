//! The Audinate ConMon envelope, and where its channels live on the wire.
//!
//! Source: `ql1-rio3224d2-pairing-gain-phantom.control.pcap` in the private
//! `dante-captures` repo, a QL1 pairing with a Rio3224-D2. Every constant
//! here was read off that capture; the tests carry frames from it.
//!
//! ```text
//! offset  size  field
//!      0     2  magic: 0xffff, or 0xfffe on the monitoring channel (8708)
//!      2     2  total length of the UDP payload
//!      4     2  sequence, counted per channel
//!      6     2  process id (0 on everything captured)
//!      8     8  sender's device id (its MAC with ff:fe in the middle)
//!     16     8  vendor id, ASCII ("Audinate")
//!     24     2  sender's software version (0x0734 / 0x0731 from the Rio)
//!     26     2  message type
//!     28     .  body
//! ```
//!
//! Bytes 24..28 are kept together as `class`, which is how the rest of this
//! workspace already names them (`0x07311002` is "a Rio's vendor message").

use std::net::Ipv4Addr;

/// Unicast queries to a device ("send me your status of type N").
pub const CONTROL_PORT: u16 = 8700;
/// Device status: clock, versions, sample rate, make and model.
pub const STATUS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 231);
pub const STATUS_PORT: u16 = 8702;
/// Vendor messages. Yamaha's head-amp protocol travels here.
pub const VENDOR_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 232);
pub const VENDOR_PORT: u16 = 8705;
/// Once-a-second monitoring heartbeat (magic `0xfffe`).
pub const MONITOR_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 233);
pub const MONITOR_PORT: u16 = 8708;
/// ConMon registration: a controller asks a device to send it a channel.
pub const CMC_PORT: u16 = 8800;

pub const MAGIC: u16 = 0xffff;
pub const MAGIC_MONITOR: u16 = 0xfffe;
pub const VENDOR_AUDINATE: [u8; 8] = *b"Audinate";
pub const HEADER_LEN: usize = 28;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub magic: u16,
    pub sequence: u16,
    pub process: u16,
    pub device_id: [u8; 8],
    pub vendor: [u8; 8],
    /// Software version in the high half, message type in the low half.
    pub class: u32,
}

impl Header {
    pub fn message_type(&self) -> u16 {
        self.class as u16
    }
}

/// Builds a ConMon message. The length field is filled in.
pub fn encode(header: &Header, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&header.magic.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&header.sequence.to_be_bytes());
    out.extend_from_slice(&header.process.to_be_bytes());
    out.extend_from_slice(&header.device_id);
    out.extend_from_slice(&header.vendor);
    out.extend_from_slice(&header.class.to_be_bytes());
    out.extend_from_slice(body);
    let total = out.len() as u16;
    out[2..4].copy_from_slice(&total.to_be_bytes());
    out
}

/// Splits a UDP payload into its ConMon header and body. `None` for anything
/// that is not a ConMon message; a length field that disagrees with the
/// datagram is tolerated, because the datagram is what arrived.
pub fn parse(payload: &[u8]) -> Option<(Header, &[u8])> {
    if payload.len() < HEADER_LEN {
        return None;
    }
    let magic = u16::from_be_bytes([payload[0], payload[1]]);
    if magic != MAGIC && magic != MAGIC_MONITOR {
        return None;
    }
    let header = Header {
        magic,
        sequence: u16::from_be_bytes([payload[4], payload[5]]),
        process: u16::from_be_bytes([payload[6], payload[7]]),
        device_id: payload[8..16].try_into().ok()?,
        vendor: payload[16..24].try_into().ok()?,
        class: u32::from_be_bytes(payload[24..28].try_into().ok()?),
    };
    Some((header, &payload[HEADER_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexutil::hex;

    /// Frame 596 of the reference capture: the Rio's sample-rate status.
    const RIO_SAMPLE_RATE: &str = "ffff004000130000001dc1fffe25df04417564696e61746507340080\
        00000000001800040000bb8000000000000200000000ac440000bb800001588800017700";

    #[test]
    fn parses_a_captured_status_message() {
        let frame = hex(RIO_SAMPLE_RATE);
        let (header, body) = parse(&frame).unwrap();
        assert_eq!(header.magic, MAGIC);
        assert_eq!(header.sequence, 0x0013);
        assert_eq!(header.device_id, [0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x25, 0xdf, 0x04]);
        assert_eq!(header.vendor, VENDOR_AUDINATE);
        assert_eq!(header.class, 0x0734_0080);
        assert_eq!(header.message_type(), 0x0080);
        assert_eq!(body.len(), frame.len() - HEADER_LEN);
    }

    #[test]
    fn encoding_a_parsed_message_is_byte_identical() {
        let frame = hex(RIO_SAMPLE_RATE);
        let (header, body) = parse(&frame).unwrap();
        assert_eq!(encode(&header, body), frame);
    }

    #[test]
    fn anything_else_is_not_conmon() {
        assert!(parse(&[0u8; 10]).is_none());
        assert!(parse(&[0x27u8; 40]).is_none());
    }
}

//! ConMon registration (UDP 8800): how a controller asks a device to send
//! it one of its ConMon channels.
//!
//! Source: the QL1 / Rio3224-D2 capture. The console sent two kinds of
//! request, and the Rio's answers are what [`respond`] reproduces.
//!
//! ```text
//! offset  size  field
//!      0     2  0x1200
//!      2     2  total length
//!      4     2  transaction id, echoed
//!      6     2  opcode
//!      8     2  result: 0 in a request, 1 in the answer
//!     10     2  the requester's ConMon process id; zero in the answer
//!     12     8  sender's device id
//!     20     .  body
//! ```
//!
//! **`0x1001`, "where is your control port?"** The request has no body. The
//! answer's body is `0001 0000 | device address | control port | 0000`.
//!
//! **`0x3010`, "send me this channel".** The body names the device and the
//! subscriber and gives the address to send to. The console repeated it
//! about once a second for each channel it wanted (channel 2, status, to its
//! port 8702; channel 6 to its port 8706), and sent it once more with a zero
//! address when it let go. The Rio's answer is the request sent back with
//! the result set and its own device id.
//!
//! Pointers in a `0x3010` body count from the start of the body (offset 20).

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::identity::DeviceIdentity;

pub const MAGIC: u16 = 0x1200;
pub const OP_LOCATE: u16 = 0x1001;
pub const OP_SUBSCRIBE: u16 = 0x3010;
/// The ConMon status channel, the one a virtual device has something to
/// send on.
pub const CHANNEL_STATUS: u16 = 2;

const HEADER_LEN: usize = 20;

/// A controller's request to be sent a channel. A zero address withdraws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription {
    pub channel: u16,
    pub address: SocketAddrV4,
}

impl Subscription {
    pub fn is_withdrawal(&self) -> bool {
        self.address.ip().is_unspecified() || self.address.port() == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub packet: Vec<u8>,
    /// Set for `0x3010`: what the controller asked for.
    pub subscription: Option<Subscription>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

/// Answers a registration request, or `None` for anything unrecognised
/// (a real device's answer to an unknown opcode was never captured, so
/// none is invented).
pub fn respond(request: &[u8], identity: &DeviceIdentity, control_port: u16) -> Option<Reply> {
    if request.len() < HEADER_LEN || u16_at(request, 0)? != MAGIC {
        return None;
    }
    // A reply to somebody else's request, or our own echoed back.
    if u16_at(request, 8)? != 0 {
        return None;
    }
    let opcode = u16_at(request, 6)?;
    let mut packet = request[..HEADER_LEN].to_vec();
    packet[8..10].copy_from_slice(&1u16.to_be_bytes());
    // The console left this word zero; Dante Controller puts its process
    // id in it, and a real module answers with it cleared.
    packet[10..12].fill(0);
    packet[12..20].copy_from_slice(&identity.eui64());

    match opcode {
        OP_LOCATE => {
            packet.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
            packet.extend_from_slice(&identity.ip.octets());
            packet.extend_from_slice(&control_port.to_be_bytes());
            packet.extend_from_slice(&[0x00, 0x00]);
            let total = packet.len() as u16;
            packet[2..4].copy_from_slice(&total.to_be_bytes());
            Some(Reply { packet, subscription: None })
        }
        OP_SUBSCRIBE => {
            packet.extend_from_slice(&request[HEADER_LEN..]);
            Some(Reply { packet, subscription: parse_subscription(request) })
        }
        _ => None,
    }
}

/// Follows the body's pointers to the channel and the address to send it to.
fn parse_subscription(request: &[u8]) -> Option<Subscription> {
    let body = request.get(HEADER_LEN..)?;
    // body+6: pointer to the list of channel entries.
    let entry = u16_at(body, 6)? as usize;
    // entry+2: pointer to the channel record.
    let record = u16_at(body, entry + 2)? as usize;
    let channel = u16_at(body, record)?;
    let ip = body.get(record + 8..record + 12)?;
    let port = u16_at(body, record + 12)?;
    Some(Subscription {
        channel,
        address: SocketAddrV4::new(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]), port),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexutil::hex;

    fn rio() -> DeviceIdentity {
        DeviceIdentity {
            name: "Y004-Yamaha-Rio3224-D2-25df04".into(),
            mac: [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04],
            ip: "169.254.193.229".parse().unwrap(),
        }
    }

    // Frames 650/651: the console registers for the status channel, and the
    // Rio's answer.
    const SUBSCRIBE_REQUEST: &str = "120000680000301000000000001dc117ea2c00000004002800010040000a\
        593030342d59616d6168612d52696f333232342d44322d32356466303400\
        593030312d59616d6168612d514c312d3137656132630000000100440002000000010000a9feb10d21fe0000";
    const SUBSCRIBE_REPLY: &str = "120000680000301000010000001dc1fffe25df040004002800010040000a\
        593030342d59616d6168612d52696f333232342d44322d32356466303400\
        593030312d59616d6168612d514c312d3137656132630000000100440002000000010000a9feb10d21fe0000";

    // Frames 663/664.
    const LOCATE_REQUEST: &str = "120000140002100100000000001dc117ea2c0000";
    const LOCATE_REPLY: &str = "120000200002100100010000001dc1fffe25df0400010000a9fec1e521fc0000";

    #[test]
    fn locate_gives_the_rios_own_answer() {
        let reply = respond(&hex(LOCATE_REQUEST), &rio(), 8700).unwrap();
        assert_eq!(reply.packet, hex(LOCATE_REPLY));
        assert_eq!(reply.subscription, None);
    }

    #[test]
    fn subscribe_gives_the_rios_own_answer_and_the_address_to_send_to() {
        let reply = respond(&hex(SUBSCRIBE_REQUEST), &rio(), 8700).unwrap();
        assert_eq!(reply.packet, hex(SUBSCRIBE_REPLY));
        assert_eq!(
            reply.subscription,
            Some(Subscription {
                channel: CHANNEL_STATUS,
                address: "169.254.177.13:8702".parse().unwrap(),
            })
        );
    }

    #[test]
    fn the_requesters_process_id_is_not_echoed() {
        // Dante Controller's form of the "where is your control port?"
        // request (frame 1 of `yamaha-dm3/dm3-dante-cmc-arc.pcapng`, with the
        // requester's id replaced): bytes 10..12 carry its process id. The
        // DM3's module answered with that word zero.
        let request = hex("120000140000100100001b9c02aabbccddee0000");
        let reply = respond(&request, &rio(), 8700).unwrap();
        assert_eq!(reply.packet, hex("120000200000100100010000001dc1fffe25df0400010000a9fec1e521fc0000"));
    }

    #[test]
    fn a_zero_address_is_a_withdrawal() {
        let mut request = hex(SUBSCRIBE_REQUEST);
        let n = request.len();
        request[n - 8..n - 2].fill(0);
        let subscription = respond(&request, &rio(), 8700).unwrap().subscription.unwrap();
        assert!(subscription.is_withdrawal());
    }

    #[test]
    fn replies_and_strangers_get_no_answer() {
        assert!(respond(&hex(SUBSCRIBE_REPLY), &rio(), 8700).is_none(), "a reply is not a request");
        assert!(respond(&[0u8; 20], &rio(), 8700).is_none());
        let mut unknown = hex(LOCATE_REQUEST);
        unknown[6..8].copy_from_slice(&0x7777u16.to_be_bytes());
        assert!(respond(&unknown, &rio(), 8700).is_none());
    }

    #[test]
    fn a_truncated_subscribe_is_echoed_without_a_subscription() {
        let request = hex(SUBSCRIBE_REQUEST);
        let reply = respond(&request[..40], &rio(), 8700).unwrap();
        assert_eq!(reply.subscription, None);
    }
}

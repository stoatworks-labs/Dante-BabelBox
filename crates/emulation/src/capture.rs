//! Reads UDP datagrams out of a packet capture, for replaying a real
//! controller's requests at this crate's responders.
//!
//! This is test tooling, kept in the library so the vendor crates' replay
//! examples can share it. It reads classic pcap and pcapng (`tcpdump -w` on
//! macOS writes pcapng even into a file named `.pcap`), Ethernet / IPv4 /
//! UDP only. Anything else in the file is skipped, not guessed at.

use std::net::{Ipv4Addr, SocketAddrV4};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    /// Microseconds, on the capture's own clock.
    pub time_us: u64,
    pub source: SocketAddrV4,
    pub destination: SocketAddrV4,
    pub payload: Vec<u8>,
}

/// Every UDP datagram in a capture file, in capture order.
pub fn datagrams(file: &[u8]) -> Result<Vec<Datagram>, String> {
    let frames = match file.get(..4) {
        Some([0x0a, 0x0d, 0x0d, 0x0a]) => pcapng_frames(file)?,
        Some([0xd4, 0xc3, 0xb2, 0xa1]) => pcap_frames(file, true),
        Some([0xa1, 0xb2, 0xc3, 0xd4]) => pcap_frames(file, false),
        _ => return Err("not a pcap or pcapng file".to_string()),
    };
    Ok(frames.into_iter().filter_map(|(time_us, frame)| udp(time_us, frame)).collect())
}

fn pcap_frames(bytes: &[u8], little_endian: bool) -> Vec<(u64, &[u8])> {
    let u32_at = |at: usize| -> Option<u32> {
        let raw = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if little_endian { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
    };
    let mut out = Vec::new();
    let mut offset = 24; // global header
    while let (Some(sec), Some(usec), Some(captured)) = (u32_at(offset), u32_at(offset + 4), u32_at(offset + 8)) {
        let start = offset + 16;
        let end = start + captured as usize;
        let Some(frame) = bytes.get(start..end) else { break };
        out.push((u64::from(sec) * 1_000_000 + u64::from(usec), frame));
        offset = end;
    }
    out
}

fn pcapng_frames(bytes: &[u8]) -> Result<Vec<(u64, &[u8])>, String> {
    const SECTION_HEADER: u32 = 0x0a0d_0d0a;
    const ENHANCED_PACKET: u32 = 6;

    let mut out = Vec::new();
    let mut offset = 0usize;
    // A section declares its own byte order; checked, not assumed.
    let mut little_endian = true;
    while offset + 12 <= bytes.len() {
        let u32_at = |at: usize, le: bool| -> Option<u32> {
            let raw = bytes.get(at..at + 4)?.try_into().ok()?;
            Some(if le { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) })
        };
        let block_type = u32_at(offset, little_endian).ok_or("truncated block")?;
        if block_type == SECTION_HEADER {
            little_endian = u32_at(offset + 8, true) == Some(0x1a2b_3c4d);
        }
        let block_len = u32_at(offset + 4, little_endian).ok_or("truncated block")? as usize;
        if block_len < 12 || offset + block_len > bytes.len() {
            break;
        }
        if block_type == ENHANCED_PACKET {
            let high = u64::from(u32_at(offset + 12, little_endian).unwrap_or(0));
            let low = u64::from(u32_at(offset + 16, little_endian).unwrap_or(0));
            let captured = u32_at(offset + 20, little_endian).unwrap_or(0) as usize;
            if let Some(frame) = bytes.get(offset + 28..offset + 28 + captured) {
                // The default timestamp resolution is microseconds.
                out.push(((high << 32) | low, frame));
            }
        }
        offset += block_len;
    }
    Ok(out)
}

/// Ethernet -> IPv4 -> UDP. Strict on purpose: a VLAN tag or an IPv6 frame
/// is skipped, so it cannot be misread into a payload.
fn udp(time_us: u64, frame: &[u8]) -> Option<Datagram> {
    const ETHERTYPE_IPV4: u16 = 0x0800;
    const PROTOCOL_UDP: u8 = 17;

    if u16::from_be_bytes(frame.get(12..14)?.try_into().ok()?) != ETHERTYPE_IPV4 {
        return None;
    }
    let ip = frame.get(14..)?;
    let version_ihl = *ip.first()?;
    if version_ihl >> 4 != 4 || *ip.get(9)? != PROTOCOL_UDP {
        return None;
    }
    // A fragment after the first has no UDP header to read.
    if u16::from_be_bytes([*ip.get(6)?, *ip.get(7)?]) & 0x1fff != 0 {
        return None;
    }
    let address = |at: usize| -> Option<Ipv4Addr> {
        let b = ip.get(at..at + 4)?;
        Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
    };
    let header = udp_header(ip, (version_ihl & 0x0f) as usize * 4)?;
    Some(Datagram {
        time_us,
        source: SocketAddrV4::new(address(12)?, header.0),
        destination: SocketAddrV4::new(address(16)?, header.1),
        payload: header.2.to_vec(),
    })
}

fn udp_header(ip: &[u8], ip_header_len: usize) -> Option<(u16, u16, &[u8])> {
    let udp = ip.get(ip_header_len..)?;
    let source = u16::from_be_bytes([*udp.first()?, *udp.get(1)?]);
    let destination = u16::from_be_bytes([*udp.get(2)?, *udp.get(3)?]);
    // The UDP length covers its own 8-byte header.
    let length = u16::from_be_bytes([*udp.get(4)?, *udp.get(5)?]) as usize;
    Some((source, destination, udp.get(8..length.max(8))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A classic little-endian pcap holding one Ethernet/IPv4/UDP frame.
    fn one_datagram_pcap(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; 12];
        frame.extend_from_slice(&[0x08, 0x00]);
        let total = (20 + 8 + payload.len()) as u16;
        frame.extend_from_slice(&[0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0, 0, 1, 17, 0, 0]);
        frame.extend_from_slice(&[169, 254, 177, 13]);
        frame.extend_from_slice(&[169, 254, 193, 229]);
        let udp_len = (8 + payload.len()) as u16;
        frame.extend_from_slice(&52180u16.to_be_bytes());
        frame.extend_from_slice(&4440u16.to_be_bytes());
        frame.extend_from_slice(&udp_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(payload);

        let mut file = vec![0xd4, 0xc3, 0xb2, 0xa1];
        file.extend_from_slice(&[0u8; 20]);
        file.extend_from_slice(&7u32.to_le_bytes());
        file.extend_from_slice(&250_000u32.to_le_bytes());
        file.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        file.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        file.extend_from_slice(&frame);
        file
    }

    #[test]
    fn reads_a_udp_datagram_out_of_a_classic_pcap() {
        let got = datagrams(&one_datagram_pcap(b"\x28\x01\x00\x0a")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].time_us, 7_250_000);
        assert_eq!(got[0].source, "169.254.177.13:52180".parse().unwrap());
        assert_eq!(got[0].destination, "169.254.193.229:4440".parse().unwrap());
        assert_eq!(got[0].payload, b"\x28\x01\x00\x0a");
    }

    #[test]
    fn a_file_that_is_not_a_capture_is_an_error_and_a_cut_one_is_not_a_panic() {
        assert!(datagrams(b"hello world").is_err());
        let file = one_datagram_pcap(b"abcd");
        for cut in 0..file.len() {
            let _ = datagrams(&file[..cut]);
        }
    }
}

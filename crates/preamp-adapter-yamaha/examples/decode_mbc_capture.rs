//! Replays a packet capture through the real `MBC` decoder and prints the
//! head-amp state it recovers.
//!
//! This is the honest way to exercise the Yamaha R-series support without
//! a QL1 and a Rio3224-D2 on the desk: the packets are real traffic
//! recorded off real hardware, and everything downstream of the socket is
//! the same code the adapter runs.
//!
//! ```text
//! cargo run -p dante-babelbox-preamp-adapter-yamaha --example decode_mbc_capture -- <capture.pcap>
//! ```
//!
//! Captures live in the private `dante-captures` repo. Pass `--realtime`
//! to pace playback by the capture's own timestamps instead of decoding as
//! fast as the file can be read.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use dante_babelbox_preamp_adapter_yamaha::mbc::codec::{
    blocks_in_conmon_payload, MbcBlock, HEADAMP_CHANNELS, OPCODE_HEADAMP, OPCODE_METERING,
    SUBOP_GAIN, SUBOP_PHANTOM,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let realtime = args.iter().any(|a| a == "--realtime");
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: decode_mbc_capture <capture.pcap|.pcapng> [--realtime]");
        return ExitCode::FAILURE;
    };

    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("could not read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let packets = match read_packets(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("{path}");
    println!("{} packets", packets.len());
    println!();

    let mut state = Replay::default();
    let mut previous_ts: Option<u64> = None;

    for packet in &packets {
        let Some(payload) = udp_payload(&packet.data) else {
            continue;
        };
        let blocks = blocks_in_conmon_payload(payload);
        if blocks.is_empty() {
            continue;
        }
        if realtime {
            if let Some(previous) = previous_ts {
                let gap = packet.timestamp_us.saturating_sub(previous);
                // Metering runs at 31 Hz; capping keeps a long idle stretch
                // from stalling playback for minutes.
                thread::sleep(Duration::from_micros(gap.min(2_000_000)));
            }
            previous_ts = Some(packet.timestamp_us);
        }
        for block in blocks {
            state.apply(&block, packet.timestamp_us);
        }
    }

    state.report();
    if state.mbc_blocks == 0 {
        eprintln!("\nno MBC blocks found - is this a Yamaha head-amp capture?");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[derive(Default)]
struct Replay {
    mbc_blocks: u64,
    gain_updates: u64,
    phantom_updates: u64,
    metering_updates: u64,
    queries: u64,
    gain_db: Option<[f32; HEADAMP_CHANNELS]>,
    phantom: Option<[bool; HEADAMP_CHANNELS]>,
    /// Opcode/subop pairs seen that this decoder has no meaning for, with
    /// the element count and width recovered from the data.
    unmapped: BTreeMap<(u16, u8), Unmapped>,
    first_ts: Option<u64>,
}

impl Replay {
    fn apply(&mut self, block: &MbcBlock, timestamp_us: u64) {
        self.mbc_blocks += 1;
        let start = *self.first_ts.get_or_insert(timestamp_us);
        let seconds = (timestamp_us.saturating_sub(start)) as f64 / 1e6;

        if block.is_query() {
            self.queries += 1;
            return;
        }

        if let Some(gains) = block.gain_db() {
            self.gain_updates += 1;
            let previous = self.gain_db;
            let mut array = [0.0f32; HEADAMP_CHANNELS];
            array.copy_from_slice(&gains);
            if let Some(previous) = previous {
                for (i, (old, new)) in previous.iter().zip(array.iter()).enumerate() {
                    if old != new {
                        println!("{seconds:7.2}s  input {:>2}  gain {old:+6.2} -> {new:+6.2} dB", i + 1);
                    }
                }
            } else {
                println!(
                    "{seconds:7.2}s  gain array first seen: {} channels, {:+.2} .. {:+.2} dB",
                    array.len(),
                    array.iter().cloned().fold(f32::INFINITY, f32::min),
                    array.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                );
            }
            self.gain_db = Some(array);
            return;
        }

        if let Some(phantom) = block.phantom() {
            self.phantom_updates += 1;
            let previous = self.phantom;
            let mut array = [false; HEADAMP_CHANNELS];
            array.copy_from_slice(&phantom);
            if let Some(previous) = previous {
                for (i, (old, new)) in previous.iter().zip(array.iter()).enumerate() {
                    if old != new {
                        println!(
                            "{seconds:7.2}s  input {:>2}  +48V {}",
                            i + 1,
                            if *new { "ON" } else { "off" }
                        );
                    }
                }
            } else {
                let on = array.iter().filter(|&&p| p).count();
                println!("{seconds:7.2}s  phantom array first seen: {on} of {} on", array.len());
            }
            self.phantom = Some(array);
            return;
        }

        if block.metering_raw().is_some() {
            self.metering_updates += 1;
            return;
        }

        let entry = self.unmapped.entry((block.opcode, block.subop)).or_default();
        entry.blocks += 1;
        if entry.width.is_none() {
            entry.width = block.element_width().ok().flatten();
            entry.count = block.count;
        }
    }

    fn report(&self) {
        println!();
        println!("decoded {} MBC blocks", self.mbc_blocks);
        println!("  gain updates      {}", self.gain_updates);
        println!("  phantom updates   {}", self.phantom_updates);
        println!("  metering updates  {}", self.metering_updates);
        println!("  read requests     {}", self.queries);

        if let Some(gains) = self.gain_db {
            println!();
            println!("final head-amp state");
            let phantom = self.phantom.unwrap_or([false; HEADAMP_CHANNELS]);
            for row in 0..HEADAMP_CHANNELS / 8 {
                let cells: Vec<String> = (0..8)
                    .map(|col| {
                        let i = row * 8 + col;
                        format!(
                            "{:>2}:{:+6.2}{}",
                            i + 1,
                            gains[i],
                            if phantom[i] { "*" } else { " " }
                        )
                    })
                    .collect();
                println!("  {}", cells.join("  "));
            }
            println!("  (* = +48V on)");
        }

        if !self.unmapped.is_empty() {
            println!();
            println!("blocks with no decoded meaning - shape only, deliberately not interpreted:");
            for ((opcode, subop), seen) in &self.unmapped {
                let shape = match seen.width {
                    Some(1) => format!("{} x uint8", seen.count),
                    Some(2) => format!("{} x int16", seen.count),
                    Some(w) => format!("{} x {w} bytes", seen.count),
                    None => "no data".to_string(),
                };
                println!("  {opcode:#06x}/{subop:02x}  {:>5} blocks  {shape}", seen.blocks);
            }
        }
        let _ = (OPCODE_HEADAMP, OPCODE_METERING, SUBOP_GAIN, SUBOP_PHANTOM);
    }
}

/// Shape of a block class this decoder deliberately does not interpret.
#[derive(Default)]
struct Unmapped {
    blocks: u64,
    count: u16,
    width: Option<usize>,
}

struct Packet {
    timestamp_us: u64,
    data: Vec<u8>,
}

/// Reads either a classic pcap or a pcapng. `tcpdump -w` on macOS writes
/// pcapng even when the file is named `.pcap`, which is worth knowing
/// before reaching for a classic-only parser.
fn read_packets(bytes: &[u8]) -> Result<Vec<Packet>, String> {
    match bytes.get(..4) {
        Some([0x0a, 0x0d, 0x0d, 0x0a]) => read_pcapng(bytes),
        Some([0xd4, 0xc3, 0xb2, 0xa1]) => read_pcap(bytes, true),
        Some([0xa1, 0xb2, 0xc3, 0xd4]) => read_pcap(bytes, false),
        _ => Err("not a pcap or pcapng file".to_string()),
    }
}

fn read_pcap(bytes: &[u8], swapped: bool) -> Result<Vec<Packet>, String> {
    let u32_at = |o: usize| -> Option<u32> {
        let raw = bytes.get(o..o + 4)?.try_into().ok()?;
        Some(if swapped {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    };
    let mut out = Vec::new();
    let mut offset = 24; // global header
    while offset + 16 <= bytes.len() {
        let (Some(sec), Some(usec), Some(incl)) =
            (u32_at(offset), u32_at(offset + 4), u32_at(offset + 8))
        else {
            break;
        };
        let start = offset + 16;
        let end = start + incl as usize;
        if end > bytes.len() {
            break;
        }
        out.push(Packet {
            timestamp_us: sec as u64 * 1_000_000 + usec as u64,
            data: bytes[start..end].to_vec(),
        });
        offset = end;
    }
    Ok(out)
}

fn read_pcapng(bytes: &[u8]) -> Result<Vec<Packet>, String> {
    const SECTION_HEADER: u32 = 0x0A0D_0D0A;
    const ENHANCED_PACKET: u32 = 0x0000_0006;
    const SIMPLE_PACKET: u32 = 0x0000_0003;

    let mut out = Vec::new();
    let mut offset = 0usize;
    // Section header byte order is declared by its own magic; every
    // capture this tool has seen is little-endian, and the byte-order
    // magic is checked rather than assumed.
    let mut little_endian = true;

    while offset + 12 <= bytes.len() {
        let read_u32 = |o: usize, le: bool| -> Option<u32> {
            let raw = bytes.get(o..o + 4)?.try_into().ok()?;
            Some(if le {
                u32::from_le_bytes(raw)
            } else {
                u32::from_be_bytes(raw)
            })
        };
        let block_type = read_u32(offset, little_endian).ok_or("truncated block type")?;

        if block_type == SECTION_HEADER {
            let magic = read_u32(offset + 8, true).ok_or("truncated section header")?;
            little_endian = magic == 0x1A2B_3C4D;
        }

        let block_len = read_u32(offset + 4, little_endian).ok_or("truncated block length")? as usize;
        if block_len < 12 || offset + block_len > bytes.len() {
            break;
        }

        match block_type {
            ENHANCED_PACKET => {
                let high = read_u32(offset + 12, little_endian).unwrap_or(0) as u64;
                let low = read_u32(offset + 16, little_endian).unwrap_or(0) as u64;
                let captured = read_u32(offset + 20, little_endian).unwrap_or(0) as usize;
                let start = offset + 28;
                let end = start + captured;
                if end <= offset + block_len {
                    out.push(Packet {
                        // Default if_tsresol is 10^-6, i.e. microseconds.
                        timestamp_us: (high << 32) | low,
                        data: bytes[start..end].to_vec(),
                    });
                }
            }
            SIMPLE_PACKET => {
                let start = offset + 12;
                let end = offset + block_len - 4;
                if start < end {
                    out.push(Packet {
                        timestamp_us: 0,
                        data: bytes[start..end].to_vec(),
                    });
                }
            }
            _ => {}
        }
        offset += block_len;
    }
    Ok(out)
}

/// Ethernet -> IPv4 -> UDP, returning the UDP payload.
///
/// Deliberately strict: anything that is not plain IPv4/UDP is skipped
/// rather than guessed at, so a VLAN tag or IPv6 frame cannot be
/// misparsed into a payload that then fails a checksum for the wrong
/// reason.
fn udp_payload(frame: &[u8]) -> Option<&[u8]> {
    const ETHERTYPE_IPV4: u16 = 0x0800;
    const PROTOCOL_UDP: u8 = 17;

    let ethertype = u16::from_be_bytes(frame.get(12..14)?.try_into().ok()?);
    if ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let ip = frame.get(14..)?;
    let version_ihl = *ip.first()?;
    if version_ihl >> 4 != 4 {
        return None;
    }
    let header_len = (version_ihl & 0x0F) as usize * 4;
    if *ip.get(9)? != PROTOCOL_UDP {
        return None;
    }
    let udp = ip.get(header_len..)?;
    let length = u16::from_be_bytes(udp.get(4..6)?.try_into().ok()?) as usize;
    // The UDP length covers its own 8-byte header.
    udp.get(8..length.max(8))
}

//! Replays a packet capture through the real ACN/DMP decoder and prints
//! the receiver telemetry it recovers.
//!
//! ```text
//! cargo run -p dante-babelbox-mic-adapter-shure-acn --example decode_capture -- <capture.pcap>
//! ```
//!
//! Captures live in the private `dante-captures` repo. This is the only
//! way to exercise the ACN path without a QLX-D mounted on a Yamaha
//! console *and* a mirrored port, because the receiver unicasts its events
//! to the console rather than multicasting them.
//!
//! The pcap reader here is deliberately a local copy of the one in
//! `preamp-adapter-yamaha`'s example of the same name: examples are not
//! part of either crate's public surface, and sharing it would mean one
//! crate depending on the other purely for test tooling.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process::ExitCode;

use dante_babelbox_mic_adapter_shure_acn::acn::{
    properties_in_datagram, Value, NO_RECENT_DATA, PROP_BATTERY_BARS, PROP_CHANNEL_NAME,
    PROP_DEVICE_NAME, PROP_FREQUENCY_KHZ, PROP_MODEL_NAME, PROP_RF_BARS, PROP_RF_LEVEL_DBM,
    PROP_UNRESOLVED_INDICATOR, PROP_UNRESOLVED_LEVEL,
};
use dante_babelbox_mic_adapter_shure_acn::slp::parse_attr_reply;

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: decode_capture <capture.pcap|.pcapng>");
        return ExitCode::FAILURE;
    };
    let bytes = match fs::read(&path) {
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

    let mut seen_devices: BTreeMap<String, String> = BTreeMap::new();
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut first_ts: Option<u64> = None;

    let mut battery: Option<i8> = None;
    let mut rf_bars: Option<u8> = None;
    let mut rf_dbm: Option<i32> = None;
    let mut frequency: Option<u32> = None;
    let mut device_name: Option<String> = None;
    let mut channel_name: Option<String> = None;
    let mut model: Option<String> = None;
    let mut unresolved_seen = 0u64;

    for packet in &packets {
        let Some(payload) = udp_payload(&packet.data) else {
            continue;
        };

        if let Some(ad) = parse_attr_reply(payload) {
            if let (Some(m), Some(name)) = (ad.model.clone(), ad.user_name.clone()) {
                if seen_devices.insert(m.clone(), name.clone()).is_none() {
                    println!("SLP discovery: {m} named \"{name}\" at {:?}", ad.sdt_endpoint);
                }
            }
            continue;
        }

        let reports = properties_in_datagram(payload);
        if reports.is_empty() {
            continue;
        }
        let start = *first_ts.get_or_insert(packet.timestamp_us);
        let seconds = packet.timestamp_us.saturating_sub(start) as f64 / 1e6;

        for report in reports {
            match (report.address, report.value) {
                (PROP_MODEL_NAME, Value::Text(t)) => model = Some(t),
                (PROP_DEVICE_NAME, Value::Text(t)) => {
                    if device_name.as_ref() != Some(&t) {
                        println!("{seconds:7.2}s  device name  \"{t}\"");
                    }
                    device_name = Some(t);
                }
                (PROP_CHANNEL_NAME, Value::Text(t)) => {
                    if channel_name.as_ref() != Some(&t) {
                        println!("{seconds:7.2}s  channel name \"{t}\"");
                    }
                    channel_name = Some(t);
                }
                (PROP_FREQUENCY_KHZ, Value::UInt32(khz)) => {
                    if frequency != Some(khz) {
                        println!("{seconds:7.2}s  frequency    {:.3} MHz", khz as f64 / 1000.0);
                    }
                    frequency = Some(khz);
                }
                (PROP_RF_BARS, Value::UInt8(bars)) => {
                    if rf_bars != Some(bars) {
                        println!("{seconds:7.2}s  RF bars      {bars}/5");
                    }
                    rf_bars = Some(bars);
                    *counts.entry("rf").or_default() += 1;
                }
                (PROP_RF_LEVEL_DBM, Value::Int32(dbm)) => {
                    if rf_dbm != Some(dbm) {
                        println!("{seconds:7.2}s  RF level     {dbm} dBm");
                    }
                    rf_dbm = Some(dbm);
                }
                (PROP_BATTERY_BARS, Value::Int8(bars)) => {
                    if battery != Some(bars) {
                        if bars == NO_RECENT_DATA {
                            println!("{seconds:7.2}s  battery      no recent data");
                        } else {
                            println!("{seconds:7.2}s  battery      {bars}/5 bars");
                        }
                    }
                    battery = Some(bars);
                    *counts.entry("battery").or_default() += 1;
                }
                (PROP_UNRESOLVED_LEVEL | PROP_UNRESOLVED_INDICATOR, _) => unresolved_seen += 1,
                _ => {}
            }
        }
    }

    println!();
    println!("receiver: {}", model.as_deref().unwrap_or("unknown"));
    if let Some(name) = &device_name {
        println!("  device name   {name}");
    }
    if let Some(name) = &channel_name {
        println!("  channel name  {name}");
    }
    if let Some(khz) = frequency {
        println!("  frequency     {:.3} MHz", khz as f64 / 1000.0);
    }
    match battery {
        Some(NO_RECENT_DATA) => println!("  battery       no recent data"),
        Some(bars) => println!("  battery       {bars}/5 bars"),
        None => println!("  battery       never reported"),
    }
    println!(
        "  RF            {} / {}",
        rf_dbm.map(|d| format!("{d} dBm")).unwrap_or_else(|| "-".into()),
        rf_bars.map(|b| format!("{b}/5 bars")).unwrap_or_else(|| "-".into()),
    );
    println!();
    println!("{unresolved_seen} readings of 0x02000812/0x02000815 decoded and deliberately");
    println!("left uninterpreted - they move with no carrier present, so they are not audio.");

    if counts.is_empty() {
        eprintln!("\nno ACN telemetry found - is this a QLX-D capture?");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

struct Packet {
    timestamp_us: u64,
    data: Vec<u8>,
}

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
    let mut offset = 24;
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

    let mut out = Vec::new();
    let mut offset = 0usize;
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
            little_endian = read_u32(offset + 8, true).ok_or("truncated section header")? == 0x1A2B_3C4D;
        }
        let block_len = read_u32(offset + 4, little_endian).ok_or("truncated block length")? as usize;
        if block_len < 12 || offset + block_len > bytes.len() {
            break;
        }
        if block_type == ENHANCED_PACKET {
            let high = read_u32(offset + 12, little_endian).unwrap_or(0) as u64;
            let low = read_u32(offset + 16, little_endian).unwrap_or(0) as u64;
            let captured = read_u32(offset + 20, little_endian).unwrap_or(0) as usize;
            let start = offset + 28;
            let end = start + captured;
            if end <= offset + block_len {
                out.push(Packet {
                    timestamp_us: (high << 32) | low,
                    data: bytes[start..end].to_vec(),
                });
            }
        }
        offset += block_len;
    }
    Ok(out)
}

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
    udp.get(8..length.max(8))
}

//! Puts a real console's head-amp messages, out of a packet capture, to the
//! emulated stagebox and compares every answer with the real stagebox's.
//!
//! ```text
//! cargo run -p dante-babelbox-preamp-adapter-yamaha --example replay_mbc_device -- \
//!     <capture.pcap> [--device IP] [--show-differences]
//! ```
//!
//! With no options it expects the reference capture
//! (`yamaha-ql1-rio3224d2/ql1-rio3224d2-pairing-gain-phantom.control.pcap`
//! in the private `dante-captures` repo). The stagebox's identity (MAC,
//! unit id, model, Dante name) is read from its own answers in the capture,
//! and so is the +48 V state it came up holding: a real stagebox remembers
//! its last state, and the comparison is of how it answers, not of what it
//! happened to remember.
//!
//! The emulated device has never been run against a console. This is the
//! next best thing: the questions are a real QL1's and the yardstick is a
//! real Rio3224-D2.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use std::process::ExitCode;

use dante_babelbox_emulation::capture::{self, Datagram};
use dante_babelbox_preamp_adapter_yamaha::mbc::codec::{
    blocks_in_conmon_payload, MbcBlock, CONMON_STATUS_PORT, FLAG_FROM_DEVICE, FLAG_UNSOLICITED,
    KIND_READ, KIND_WRITE, OPCODE_HEADAMP, SUBOP_GAIN, SUBOP_PHANTOM,
};
use dante_babelbox_preamp_adapter_yamaha::mbc::device::{
    HeadampDevice, HeadampIdentity, OPCODE_INPUT_COUNT, OPCODE_OUTPUT_COUNT, OPCODE_POLL, SUBOP_COUNT,
    SUBOP_POLL,
};

#[derive(Default)]
struct Tally {
    same: u32,
    different: u32,
    unanswered: u32,
    first_difference: Option<(Vec<u8>, Vec<u8>)>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn text(field: &[u8]) -> String {
    String::from_utf8_lossy(field.split(|&b| b == 0).next().unwrap_or_default()).into_owned()
}

fn main() -> ExitCode {
    let mut path = None;
    let mut device_ip: Ipv4Addr = Ipv4Addr::new(169, 254, 193, 229);
    let mut show = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" => match args.next().unwrap_or_default().parse() {
                Ok(ip) => device_ip = ip,
                Err(e) => {
                    eprintln!("--device: {e}");
                    return ExitCode::FAILURE;
                }
            },
            "--show-differences" => show = true,
            _ => path = Some(arg),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: replay_mbc_device <capture.pcap> [--device IP] [--show-differences]");
        return ExitCode::FAILURE;
    };
    let datagrams = match fs::read(&path).map_err(|e| e.to_string()).and_then(|b| capture::datagrams(&b)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Everything on the vendor channel, as (index, from the device?, block).
    let vendor: Vec<(usize, bool, MbcBlock)> = datagrams
        .iter()
        .enumerate()
        .filter(|(_, d)| d.destination.port() == CONMON_STATUS_PORT)
        .flat_map(|(i, d)| {
            let from_device = *d.source.ip() == device_ip;
            blocks_in_conmon_payload(&d.payload).into_iter().map(move |b| (i, from_device, b))
        })
        .collect();

    let Some(identity) = identity_from_capture(&vendor) else {
        eprintln!("{path}: no head-amp device at {device_ip} answers a console in this capture");
        return ExitCode::FAILURE;
    };
    println!("{path}");
    println!(
        "emulating unit {} ({}, '{}', {} in / {} out)",
        identity.unit_id, identity.model, identity.dante_name, identity.inputs, identity.outputs
    );
    let mut device = HeadampDevice::new(identity);
    restore_state(&vendor, &mut device);

    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let (mut changes, mut announced) = (0usize, 0usize);
    for (position, (_, from_device, block)) in vendor.iter().enumerate() {
        if *from_device {
            if block.flags & FLAG_UNSOLICITED != 0 && block.kind() != 3 {
                announced += 1;
            }
            continue;
        }
        let outcome = device.handle(block);
        changes += outcome.changes.len();
        let Some(real) = real_answer(&vendor, position, block) else {
            // The stagebox said nothing. So must we.
            if !outcome.replies.is_empty() {
                let tally = tallies.entry(label(block) + "  (device silent)").or_default();
                tally.different += 1;
            }
            continue;
        };
        let tally = tallies.entry(label(block)).or_default();
        match outcome.replies.first() {
            None => tally.unanswered += 1,
            Some(ours) => {
                let ours = ours.encode();
                let real_bytes = encoded_as_sent(&datagrams[real.0], &real.2);
                if ours == real_bytes {
                    tally.same += 1;
                } else {
                    tally.different += 1;
                    tally.first_difference.get_or_insert((real_bytes, ours));
                }
            }
        }
    }

    println!();
    println!("  {:<22} {:>5} {:>9} {:>10}", "console message", "same", "different", "no answer");
    for (name, tally) in &tallies {
        println!("  {:<22} {:>5} {:>9} {:>10}", name, tally.same, tally.different, tally.unanswered);
        if show {
            if let Some((real, ours)) = &tally.first_difference {
                println!("      device  {}", hex(real));
                println!("      here    {}", hex(ours));
            }
        }
    }
    let same: u32 = tallies.values().map(|t| t.same).sum();
    let all: u32 = tallies.values().map(|t| t.same + t.different + t.unanswered).sum();
    println!();
    println!("{same} of {all} answers identical to the stagebox's, byte for byte");
    println!("{changes} head-amp changes reported to the bridge");
    println!("({announced} unprompted announcements from the stagebox were not part of the comparison)");
    ExitCode::SUCCESS
}

fn label(block: &MbcBlock) -> String {
    let kind = match block.kind() {
        KIND_READ => "read",
        KIND_WRITE => "write",
        2 => "surface",
        3 => "meter",
        _ => "?",
    };
    format!("{kind:<7} {:04x}/{:02x}", block.opcode, block.subop)
}

/// The stagebox's answer to the console block at `position`: its next block
/// with the same opcode and subop and the console's kind plus "from device".
fn real_answer<'a>(
    vendor: &'a [(usize, bool, MbcBlock)],
    position: usize,
    request: &MbcBlock,
) -> Option<&'a (usize, bool, MbcBlock)> {
    vendor[position + 1..].iter().take(60).find(|(_, from_device, b)| {
        *from_device
            && b.flags == (FLAG_FROM_DEVICE | request.kind())
            && (b.opcode, b.subop, b.count, b.start_index) == (request.opcode, request.subop, request.count, request.start_index)
            && b.dst_mac == request.src_mac
    })
}

/// The block's bytes exactly as they were on the wire, cut out of the
/// datagram, so the comparison does not go through this crate's own encoder.
fn encoded_as_sent(datagram: &Datagram, block: &MbcBlock) -> Vec<u8> {
    for candidate in [block.encode_read_reply(), block.encode()] {
        if datagram.payload.windows(candidate.len()).any(|w| w == candidate.as_slice()) {
            return candidate;
        }
    }
    // Neither encoding is in the packet: report what was really there.
    datagram.payload.clone()
}

/// Reads the stagebox's identity out of its own answers.
fn identity_from_capture(vendor: &[(usize, bool, MbcBlock)]) -> Option<HeadampIdentity> {
    let answer = |opcode: u16, subop: u8| {
        vendor.iter().find_map(|(_, from_device, b)| {
            (*from_device && b.flags == (FLAG_FROM_DEVICE | KIND_READ) && (b.opcode, b.subop) == (opcode, subop))
                .then_some(b)
        })
    };
    let poll = answer(OPCODE_POLL, SUBOP_POLL)?;
    let word = |b: &MbcBlock| b.data.get(..2).map(|d| u16::from_be_bytes([d[0], d[1]]));
    Some(HeadampIdentity {
        mac: poll.src_mac,
        unit_id: word(poll)?,
        model: text(poll.data.get(2..18)?),
        dante_name: text(poll.data.get(18..50)?),
        inputs: word(answer(OPCODE_INPUT_COUNT, SUBOP_COUNT)?)?,
        outputs: word(answer(OPCODE_OUTPUT_COUNT, SUBOP_COUNT)?)?,
    })
}

/// Gain and +48 V are the state a stagebox carries over from its last
/// session. Where the console read one of them before writing it, the
/// emulated device is started with what the real one answered. Nothing else
/// is restored: every other answer has to come out of the emulated device's
/// own defaults.
fn restore_state(vendor: &[(usize, bool, MbcBlock)], device: &mut HeadampDevice) {
    for key in [(OPCODE_HEADAMP, SUBOP_GAIN), (OPCODE_HEADAMP, SUBOP_PHANTOM)] {
        let first = vendor.iter().find(|(_, from_device, b)| {
            (b.opcode, b.subop) == key
                && ((!*from_device && b.kind() == KIND_WRITE)
                    || (*from_device && b.flags == (FLAG_FROM_DEVICE | KIND_READ)))
        });
        if let Some((_, true, block)) = first {
            if device.restore(block.opcode, block.subop, block.start_index, &block.data) {
                println!("  started with the {:04x}/{:02x} state the stagebox held", block.opcode, block.subop);
            }
        }
    }
}

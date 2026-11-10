//! Puts a real controller's requests, out of a packet capture, to this
//! crate's responders, and compares each answer with what the real device
//! answered.
//!
//! This is the closest thing to a hardware test there is without the
//! hardware: the requests are a real console's, and the yardstick is a real
//! stagebox's own replies.
//!
//! ```text
//! cargo run -p dante-babelbox-emulation --example replay_capture -- \
//!     <capture.pcap> [--device IP] [--name NAME] [--mac MAC] [--cold] [--show-differences]
//!     [--tx N] [--rx N] [--rate HZ] [--property ID=HEX]...
//! ```
//!
//! With no options it expects the reference capture
//! (`yamaha-ql1-rio3224d2/ql1-rio3224d2-pairing-gain-phantom.control.pcap`
//! in the private `dante-captures` repo) and takes the Rio3224-D2 in it as
//! the device to emulate.
//!
//! The captured Rio came up already holding 24 subscriptions from earlier
//! sessions. The replay starts the virtual device in that same state, read
//! from the Rio's own first answer, so that the comparison is of how the
//! reply is built and not of who remembered what. `--cold` skips that and
//! starts with nothing subscribed, which is how a fresh virtual device
//! starts.
//!
//! A reply that still differs is not necessarily a fault. A virtual device
//! has no audio, so anything that reports a live flow differs, and the
//! summary says which those are.
//!
//! ## A capture of another device
//!
//! The reply *layouts* can be checked against any Dante device, not only a
//! Rio, by shaping the emulator like it: `--tx`, `--rx` and `--rate` set its
//! channel counts and sample rate, and `--property 8205=0007a120` puts a
//! value in an out-of-line property. For Dante Controller asking a DM3's
//! module (`yamaha-dm3/dm3-dante-cmc-arc.pcapng`):
//!
//! ```text
//! ... dm3-dante-cmc-arc.pcapng --device 169.254.237.144 --name Y001-Yamaha-DM3-a436af \
//!     --mac ac:44:f2:a4:36:af --tx 16 --rx 16 --rate 96000 --property 8205=0007a120
//! ```
//!
//! The channel and flow listings then compare byte for byte. Device info,
//! channel counts and the property values do not, and are not meant to:
//! they describe a different Dante module (a Broadway, not a Brooklyn 3).

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use std::process::ExitCode;

use dante_babelbox_emulation::arc::{self, ArcState, Subscription};
use dante_babelbox_emulation::capture::{self, Datagram};
use dante_babelbox_emulation::{cmc, conmon, parse_mac, status, DeviceIdentity, Persona};

#[derive(Default)]
struct Tally {
    same: u32,
    different: u32,
    unanswered: u32,
    first_difference: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

impl Tally {
    fn compare(&mut self, request: &[u8], real: &[u8], ours: Option<Vec<u8>>) {
        match ours {
            Some(ours) if ours == real => self.same += 1,
            Some(ours) => {
                self.different += 1;
                self.first_difference.get_or_insert((request.to_vec(), real.to_vec(), ours));
            }
            None => self.unanswered += 1,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn c_string(data: &[u8], at: usize) -> Option<String> {
    let tail = data.get(at..)?;
    let end = tail.iter().position(|&b| b == 0)?;
    String::from_utf8(tail[..end].to_vec()).ok()
}

fn main() -> ExitCode {
    let mut path = None;
    let mut device = "169.254.193.229".to_string();
    let mut name = "Y004-Yamaha-Rio3224-D2-25df04".to_string();
    let mut mac = "00:1d:c1:25:df:04".to_string();
    let (mut cold, mut show) = (false, false);
    let mut persona = Persona::yamaha_rio3224_d2();
    let mut properties: Vec<(u16, Vec<u8>)> = Vec::new();
    let mut another_device = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = |what: &str| -> Option<u32> {
            let value = args.next().and_then(|v| v.parse().ok());
            if value.is_none() {
                eprintln!("{what} needs a number");
            }
            value
        };
        match arg.as_str() {
            "--tx" | "--rx" | "--rate" => {
                let Some(value) = number(&arg) else { return ExitCode::FAILURE };
                match arg.as_str() {
                    "--tx" => persona.tx_channels = value as u16,
                    "--rx" => persona.rx_channels = value as u16,
                    _ => persona.sample_rate = value,
                }
                another_device = true;
            }
            "--property" => {
                let parsed = args.next().and_then(|text| {
                    let (id, value) = text.split_once('=')?;
                    let id = u16::from_str_radix(id, 16).ok()?;
                    let bytes: Option<Vec<u8>> = (0..value.len() / 2)
                        .map(|i| u8::from_str_radix(value.get(i * 2..i * 2 + 2)?, 16).ok())
                        .collect();
                    Some((id, bytes?))
                });
                let Some(property) = parsed else {
                    eprintln!("--property needs ID=HEX, for example 8205=0007a120");
                    return ExitCode::FAILURE;
                };
                properties.push(property);
            }
            "--device" => device = args.next().unwrap_or_default(),
            "--name" => name = args.next().unwrap_or_default(),
            "--mac" => mac = args.next().unwrap_or_default(),
            "--cold" => cold = true,
            "--show-differences" => show = true,
            _ => path = Some(arg),
        }
    }
    let Some(path) = path else {
        eprintln!(
            "usage: replay_capture <capture.pcap> [--device IP] [--name NAME] [--mac MAC] [--cold] [--show-differences]"
        );
        return ExitCode::FAILURE;
    };
    let (ip, mac) = match (device.parse::<Ipv4Addr>(), parse_mac(&mac)) {
        (Ok(ip), Ok(mac)) => (ip, mac),
        (Err(e), _) => {
            eprintln!("--device: {e}");
            return ExitCode::FAILURE;
        }
        (_, Err(e)) => {
            eprintln!("--mac: {e}");
            return ExitCode::FAILURE;
        }
    };
    let identity = DeviceIdentity { name, mac, ip };

    let datagrams = match fs::read(&path).map_err(|e| e.to_string()).and_then(|b| capture::datagrams(&b)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{path}");
    println!("{} UDP datagrams; emulating {} at {}", datagrams.len(), identity.name, identity.ip);

    let mut state = ArcState::new(&persona);
    for (id, value) in &properties {
        state.restore_property(*id, value);
    }
    if !cold {
        let restored = restore_subscriptions(&datagrams, &identity, &mut state);
        println!("started with the {restored} subscriptions the captured device already held");
    }

    let mut arc_tally: BTreeMap<u16, Tally> = BTreeMap::new();
    let mut cmc_tally: BTreeMap<u16, Tally> = BTreeMap::new();
    let mut status_tally: BTreeMap<u16, Tally> = BTreeMap::new();

    // One pass in capture order: a write on one port changes what the next
    // read on another port must say.
    for (index, request) in datagrams.iter().enumerate() {
        if *request.destination.ip() != identity.ip {
            continue;
        }
        let payload = &request.payload;
        match request.destination.port() {
            arc::DEFAULT_PORT => {
                let Some(opcode) = u16_at(payload, 6) else { continue };
                let Some(real) = real_reply(&datagrams, index, 4..6) else { continue };
                let ours = arc::respond(payload, &identity, &persona, &mut state);
                arc_tally.entry(opcode).or_default().compare(payload, &real.payload, ours);
            }
            conmon::CMC_PORT => {
                let Some(opcode) = u16_at(payload, 6) else { continue };
                let Some(real) = real_reply(&datagrams, index, 4..8) else { continue };
                let ours = cmc::respond(payload, &identity, conmon::CONTROL_PORT).map(|r| r.packet);
                cmc_tally.entry(opcode).or_default().compare(payload, &real.payload, ours);
            }
            conmon::CONTROL_PORT => {
                let Some((query, body)) = conmon::parse(payload) else { continue };
                if let Some(bits) = status::encoding_write(query.message_type(), body) {
                    state.set_encoding_bits(bits);
                }
                let tally = status_tally.entry(query.message_type()).or_default();
                let Some(status_type) = status::status_for_query(query.message_type()) else {
                    tally.unanswered += 1;
                    continue;
                };
                // The device's own next status of that type, within a second.
                let real = datagrams[index + 1..].iter().find_map(|d| {
                    if d.time_us > request.time_us + 1_000_000 {
                        return None;
                    }
                    if *d.source.ip() != identity.ip || d.destination.port() != conmon::STATUS_PORT {
                        return None;
                    }
                    let (header, body) = conmon::parse(&d.payload)?;
                    (header.message_type() == status_type).then(|| body.to_vec())
                });
                let ours = status::body(status_type, &identity, &persona, state.encoding_bits());
                match real {
                    Some(real) => tally.compare(payload, &real, ours),
                    None => tally.unanswered += 1,
                }
            }
            _ => {}
        }
    }

    report("ARC (UDP 4440)", &arc_tally, show, |opcode| match opcode {
        arc::OP_CHANNEL_COUNT | arc::OP_DEVICE_INFO | arc::OP_PROPERTY_GET if another_device => {
            "describes the captured device's own Dante module, which is not a Rio's"
        }
        arc::OP_RX_CHANNELS => "subscription state the device changed by itself as audio connected",
        arc::OP_TX_FLOWS | arc::OP_RX_FLOWS => "the device had audio flows; a virtual one has none",
        _ => "",
    });
    report("ConMon registration (UDP 8800)", &cmc_tally, show, |_| {
        if another_device {
            "this module writes its device id as MAC + 0000; a Rio's inserts ff:fe"
        } else {
            ""
        }
    });
    report(
        "ConMon status queries (UDP 8700 in, 8702 out; bodies compared)",
        &status_tally,
        show,
        |query| match query {
            _ if another_device => "status bodies are a Rio's module's; this device has another",
            0x0021 => "clock status: the device had found a leader; a virtual one runs no clock",
            _ => "",
        },
    );
    ExitCode::SUCCESS
}

/// The real device's reply to the request at `index`: the next datagram
/// back the other way carrying the same bytes at `id_at`.
fn real_reply(datagrams: &[Datagram], index: usize, id_at: std::ops::Range<usize>) -> Option<&Datagram> {
    let request = &datagrams[index];
    datagrams[index + 1..].iter().take(400).find(|d| {
        d.source == request.destination
            && d.destination == request.source
            && d.payload.get(id_at.clone()) == request.payload.get(id_at.clone())
    })
}

/// Reads the captured device's first receive-channel listing and puts those
/// subscriptions, with their state words, into `state`.
fn restore_subscriptions(datagrams: &[Datagram], identity: &DeviceIdentity, state: &mut ArcState) -> usize {
    let mut restored = 0;
    let mut pages_seen = Vec::new();
    for (index, request) in datagrams.iter().enumerate() {
        if *request.destination.ip() != identity.ip || request.destination.port() != arc::DEFAULT_PORT {
            continue;
        }
        if u16_at(&request.payload, 6) != Some(arc::OP_RX_CHANNELS) {
            continue;
        }
        let Some(start) = u16_at(&request.payload, 12) else { continue };
        if pages_seen.contains(&start) {
            break; // the listing has come round again
        }
        pages_seen.push(start);
        let Some(reply) = real_reply(datagrams, index, 4..6) else { continue };
        let reply = &reply.payload;
        let count = reply.get(11).copied().unwrap_or(0) as usize;
        for i in 0..count {
            let at = 12 + i * 20;
            let (Some(channel), Some(tx_channel), Some(tx_device)) =
                (u16_at(reply, at), u16_at(reply, at + 6), u16_at(reply, at + 8))
            else {
                continue;
            };
            if tx_channel == 0 || tx_device == 0 {
                continue;
            }
            let (Some(tx_channel), Some(tx_device), Some(word)) = (
                c_string(reply, tx_channel as usize),
                c_string(reply, tx_device as usize),
                reply.get(at + 12..at + 16),
            ) else {
                continue;
            };
            let word = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            state.restore_subscription(channel, Subscription { tx_channel, tx_device }, word);
            restored += 1;
        }
    }
    restored
}

fn report(title: &str, tallies: &BTreeMap<u16, Tally>, show: bool, note: impl Fn(u16) -> &'static str) {
    println!();
    println!("{title}");
    println!("  {:<8} {:>5} {:>9} {:>10}", "type", "same", "different", "no answer");
    for (opcode, tally) in tallies {
        println!(
            "  {:#06x}   {:>5} {:>9} {:>10}   {}",
            opcode,
            tally.same,
            tally.different,
            tally.unanswered,
            if tally.different + tally.unanswered > 0 { note(*opcode) } else { "" }
        );
        if show {
            if let Some((request, real, ours)) = &tally.first_difference {
                let at = real.iter().zip(ours).position(|(a, b)| a != b).unwrap_or(real.len().min(ours.len()));
                println!("      request {}", hex(request));
                println!("      device  {}", hex(real));
                println!("      here    {}", hex(ours));
                println!("      first difference at byte {at} (lengths {} / {})", real.len(), ours.len());
            }
        }
    }
    let same: u32 = tallies.values().map(|t| t.same).sum();
    let all: u32 = tallies.values().map(|t| t.same + t.different + t.unanswered).sum();
    println!("  {same} of {all} replies identical to the device's");
}

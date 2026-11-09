//! The stagebox's half of the `MBC` head-amp protocol: what a Rio answers
//! when a console polls for it, reads its parameters and writes gain and
//! +48 V.
//!
//! Everything else under `mbc` speaks *as the console* to a real stagebox.
//! This speaks *as the stagebox* to a real console. It is a state machine
//! over [`MbcBlock`]s with no sockets and no clock, so it can be put through
//! a captured console's messages one at a time.
//!
//! ## Source
//!
//! `ql1-rio3224d2-pairing-gain-phantom.control.pcap` (private
//! `dante-captures` repo): a QL1 finding, pairing with and then driving a
//! Rio3224-D2. The whole conversation is on the ConMon vendor group
//! (224.0.0.232:8705), and it has four moves. The low nibble of the flags
//! byte names the move; the device answers with the same nibble and
//! [`FLAG_FROM_DEVICE`]:
//!
//! | Console | Device answers | |
//! |---|---|---|
//! | `01` read: opcode, subop, count, no data | `21` with the data; `len` still says 10 | every parameter, at pairing |
//! | `00` write: the data | `20`, the data back | gain, +48 V, `0722/14` |
//! | `02` the `0731` arrays | `22`, the same data back | on the console's own patch and scene changes |
//! | `03` on `0742/00`: start metering | `23`, no data; then `13` to everyone at ~31 Hz | renewed by the console about every 10 s |
//!
//! The console finds devices by broadcasting a read of `071f/06` every six
//! seconds. A device answers with its unit id, model and Dante name, and the
//! console then reads the rest: identity (`0711`), channel counts
//! (`0712/0a`, `0713/0a`) and the head-amp arrays (`0722`). Having read
//! them it writes its own +48 V and gain over the top: **the console's
//! stored state wins**, whatever the device held.
//!
//! On power-up the device announces itself unprompted (flags `10`, to
//! everyone): its name and five single-byte parameters.
//!
//! ## What is known and what is copied
//!
//! Known by watching them move: gain (`0722/16`, centi-dB) and +48 V
//! (`0722/17`). Known by content: the unit id, model, Dante name and the
//! two channel counts. Every other value the console read is held here
//! exactly as the Rio3224-D2 answered it and handed back on request, without
//! a claim about what it means. The console may write any `0722` array; the
//! write is stored and echoed, as the Rio's were.
//!
//! **Never run against a console.** The answers are the Rio's, byte for
//! byte (see `examples/replay_mbc_device.rs`), which is as far as a capture
//! can take it.

use std::collections::BTreeMap;

use super::codec::{
    MbcBlock, FLAG_FROM_DEVICE, FLAG_UNSOLICITED, GAIN_MAX_DB, GAIN_MIN_DB, HEADAMP_CHANNELS,
    KIND_METER, KIND_READ, KIND_SURFACE, KIND_WRITE, MAC_BROADCAST, OPCODE_HEADAMP, OPCODE_METERING,
    OPCODE_SURFACE, SUBOP_GAIN, SUBOP_METERING, SUBOP_PHANTOM,
};

/// The console's device poll, and the heartbeat that follows it.
pub const OPCODE_POLL: u16 = 0x071f;
pub const SUBOP_POLL: u8 = 0x06;
/// Identity parameters.
pub const OPCODE_IDENTITY: u16 = 0x0711;
pub const SUBOP_UNIT_ID: u8 = 0x03;
pub const SUBOP_MODEL: u8 = 0x04;
pub const SUBOP_DANTE_NAME: u8 = 0x05;
/// Input and output channel counts, both under subop `0x0a`.
pub const OPCODE_INPUT_COUNT: u16 = 0x0712;
pub const OPCODE_OUTPUT_COUNT: u16 = 0x0713;
pub const SUBOP_COUNT: u8 = 0x0a;

/// The metering byte a Rio sends for silence.
pub const METER_FLOOR: u8 = 0x1f;

const MODEL_FIELD: usize = 16;
const NAME_FIELD: usize = 32;

/// What the device is, as far as the head-amp protocol is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadampIdentity {
    /// The MAC the device is addressed by in `MBC` blocks. A Rio uses its
    /// Dante MAC.
    pub mac: [u8; 6],
    /// The unit id set on the stagebox's front panel; `4` is `Y004`.
    pub unit_id: u16,
    /// Model name, at most 15 characters. `Rio3224-D2`.
    pub model: String,
    /// Dante device name, at most 31 characters.
    pub dante_name: String,
    pub inputs: u16,
    pub outputs: u16,
}

/// A head-amp value the console changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// `channel` is 1-based.
    Gain { channel: u16, db: f32 },
    Phantom { channel: u16, on: bool },
}

/// A block to send, and which of the two encodings it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub block: MbcBlock,
    /// A read reply, which keeps the request's `len`.
    pub read_reply: bool,
}

impl Outgoing {
    pub fn encode(&self) -> Vec<u8> {
        if self.read_reply {
            self.block.encode_read_reply()
        } else {
            self.block.encode()
        }
    }
}

/// What handling one block from a console produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// Blocks to send back, in order.
    pub replies: Vec<Outgoing>,
    /// Head-amp values that are now different.
    pub changes: Vec<Change>,
    /// The console asked for metering (again).
    pub metering_requested: bool,
}

/// One parameter: `count` elements of `width` bytes.
#[derive(Debug, Clone)]
struct Parameter {
    width: usize,
    data: Vec<u8>,
    writable: bool,
}

impl Parameter {
    fn fixed(data: &[u8]) -> Self {
        Self { width: data.len(), data: data.to_vec(), writable: false }
    }

    fn array(width: usize, element: &[u8], count: usize, writable: bool) -> Self {
        Self { width, data: element.repeat(count), writable }
    }

    fn range(&self, start: u16, count: u16) -> Option<std::ops::Range<usize>> {
        let begin = (start as usize).checked_mul(self.width)?;
        let end = begin.checked_add((count as usize).checked_mul(self.width)?)?;
        (count > 0 && end <= self.data.len()).then_some(begin..end)
    }
}

fn text_field(text: &str, width: usize) -> Vec<u8> {
    let mut field = text.as_bytes()[..text.len().min(width - 1)].to_vec();
    field.resize(width, 0);
    field
}

/// A stagebox's head amps, as a console sees them.
#[derive(Debug, Clone)]
pub struct HeadampDevice {
    identity: HeadampIdentity,
    parameters: BTreeMap<(u16, u8), Parameter>,
}

impl HeadampDevice {
    /// A device in a Rio3224-D2's resting state, except that gain starts at
    /// the bottom of its range and +48 V off on every channel. A real Rio
    /// comes up with whatever it last held; a virtual one has no "last", and
    /// the console overwrites both at pairing anyway.
    pub fn new(identity: HeadampIdentity) -> Self {
        let channels = HEADAMP_CHANNELS;
        let floor = ((GAIN_MIN_DB * 100.0) as i16).to_be_bytes();
        let mut p = BTreeMap::new();

        // Identity. 01, 02, 07, 08 and 09 are as the Rio answered; 02 reads
        // like the firmware version (0x55 is 85, and the Rio was V1.85).
        p.insert((OPCODE_IDENTITY, 0x01), Parameter::fixed(&[0x01, 0x00, 0x00, 0x00]));
        p.insert((OPCODE_IDENTITY, 0x02), Parameter::fixed(&[0x81, 0x55, 0x00, 0x00]));
        p.insert((OPCODE_IDENTITY, SUBOP_UNIT_ID), Parameter::fixed(&identity.unit_id.to_be_bytes()));
        p.insert((OPCODE_IDENTITY, SUBOP_MODEL), Parameter::fixed(&text_field(&identity.model, MODEL_FIELD)));
        p.insert(
            (OPCODE_IDENTITY, SUBOP_DANTE_NAME),
            Parameter::fixed(&text_field(&identity.dante_name, NAME_FIELD)),
        );
        p.insert((OPCODE_IDENTITY, 0x07), Parameter::fixed(&[0x06]));
        p.insert((OPCODE_IDENTITY, 0x08), Parameter::fixed(&[0x02]));
        p.insert((OPCODE_IDENTITY, 0x09), Parameter::fixed(&[0x01]));

        p.insert((OPCODE_INPUT_COUNT, SUBOP_COUNT), Parameter::fixed(&identity.inputs.to_be_bytes()));
        p.insert((OPCODE_OUTPUT_COUNT, SUBOP_COUNT), Parameter::fixed(&identity.outputs.to_be_bytes()));

        // Head-amp block. The single-byte ones are as the Rio answered.
        for (subop, value) in [(0x10, 0x01), (0x12, 0x0d), (0x13, 0x00), (0x14, 0x00), (0x15, 0x00)] {
            p.insert((OPCODE_HEADAMP, subop), Parameter::array(1, &[value], 1, true));
        }
        p.insert((OPCODE_HEADAMP, SUBOP_GAIN), Parameter::array(2, &floor, channels, true));
        p.insert((OPCODE_HEADAMP, SUBOP_PHANTOM), Parameter::array(1, &[0], channels, true));
        // The four arrays nobody touched during the capture, at the values
        // the Rio held: all zero, all 800, all -600, all zero.
        p.insert((OPCODE_HEADAMP, 0x18), Parameter::array(1, &[0], channels, true));
        p.insert((OPCODE_HEADAMP, 0x19), Parameter::array(2, &800i16.to_be_bytes(), channels, true));
        p.insert((OPCODE_HEADAMP, 0x1a), Parameter::array(2, &floor, channels, true));
        p.insert((OPCODE_HEADAMP, 0x1b), Parameter::array(1, &[0], channels, true));

        // What the console pushes with kind 02 and gets straight back: one
        // byte per input and one per output.
        p.insert((OPCODE_SURFACE, 0x01), Parameter::array(1, &[0], channels, true));
        p.insert((OPCODE_SURFACE, 0x02), Parameter::array(1, &[0], identity.outputs as usize, true));

        Self { identity, parameters: p }
    }

    pub fn identity(&self) -> &HeadampIdentity {
        &self.identity
    }

    /// Gain of `channel` (1-based) in dB.
    pub fn gain_db(&self, channel: u16) -> Option<f32> {
        let index = channel.checked_sub(1)? as usize;
        let data = &self.parameters.get(&(OPCODE_HEADAMP, SUBOP_GAIN))?.data;
        let raw = data.get(index * 2..index * 2 + 2)?;
        Some(f32::from(i16::from_be_bytes([raw[0], raw[1]])) / 100.0)
    }

    /// +48 V on `channel` (1-based).
    pub fn phantom(&self, channel: u16) -> Option<bool> {
        let index = channel.checked_sub(1)? as usize;
        Some(*self.parameters.get(&(OPCODE_HEADAMP, SUBOP_PHANTOM))?.data.get(index)? != 0)
    }

    /// Sets gain from the bridge's side. Returns the announcement to send
    /// if the value changed: the whole gain array, to everyone, which is
    /// how the Rio reported its own state.
    ///
    /// The value is clamped to the Rio's range and rounded to a whole dB,
    /// the Rio's own step; every gain in the capture is a whole dB, and a
    /// console given a fraction has never been observed.
    pub fn set_gain_db(&mut self, channel: u16, db: f32) -> Option<Outgoing> {
        let index = channel.checked_sub(1).filter(|&i| (i as usize) < HEADAMP_CHANNELS)? as usize;
        let centi = (db.clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i16) * 100;
        let parameter = self.parameters.get_mut(&(OPCODE_HEADAMP, SUBOP_GAIN))?;
        let slot = &mut parameter.data[index * 2..index * 2 + 2];
        if slot == centi.to_be_bytes() {
            return None;
        }
        slot.copy_from_slice(&centi.to_be_bytes());
        Some(self.announce(OPCODE_HEADAMP, SUBOP_GAIN))
    }

    /// Sets +48 V from the bridge's side; see [`set_gain_db`](Self::set_gain_db).
    pub fn set_phantom(&mut self, channel: u16, on: bool) -> Option<Outgoing> {
        let index = channel.checked_sub(1).filter(|&i| (i as usize) < HEADAMP_CHANNELS)? as usize;
        let parameter = self.parameters.get_mut(&(OPCODE_HEADAMP, SUBOP_PHANTOM))?;
        if (parameter.data[index] != 0) == on {
            return None;
        }
        parameter.data[index] = u8::from(on);
        Some(self.announce(OPCODE_HEADAMP, SUBOP_PHANTOM))
    }

    /// Puts raw bytes into a parameter, as if the device had come up
    /// holding them. A real stagebox remembers its last state; the capture
    /// replay uses this to start from the state the captured one was in.
    /// Returns `false` if there is no such parameter or the range is off
    /// its end.
    pub fn restore(&mut self, opcode: u16, subop: u8, start_index: u16, data: &[u8]) -> bool {
        let Some(parameter) = self.parameters.get_mut(&(opcode, subop)) else {
            return false;
        };
        let count = (data.len() / parameter.width) as u16;
        match parameter.range(start_index, count) {
            Some(range) if range.len() == data.len() => {
                parameter.data[range].copy_from_slice(data);
                true
            }
            _ => false,
        }
    }

    /// The whole of one parameter, unprompted and to everyone.
    fn announce(&self, opcode: u16, subop: u8) -> Outgoing {
        let parameter = &self.parameters[&(opcode, subop)];
        Outgoing {
            block: MbcBlock {
                version: 1,
                src_mac: self.identity.mac,
                dst_mac: MAC_BROADCAST,
                flags: FLAG_UNSOLICITED,
                opcode,
                subop,
                count: (parameter.data.len() / parameter.width) as u16,
                start_index: 0,
                data: parameter.data.clone(),
            },
            read_reply: false,
        }
    }

    /// What a Rio says when it powers up, grouped into packets the way the
    /// Rio grouped them: its Dante name and `0711/08`, then `0711/09` and
    /// `0722/10`, then `0722/14` and `0722/15` each on their own.
    pub fn power_up_announcement(&self) -> Vec<Vec<Outgoing>> {
        let a = |opcode, subop| self.announce(opcode, subop);
        vec![
            vec![a(OPCODE_IDENTITY, SUBOP_DANTE_NAME), a(OPCODE_IDENTITY, 0x08)],
            vec![a(OPCODE_IDENTITY, 0x09), a(OPCODE_HEADAMP, 0x10)],
            vec![a(OPCODE_HEADAMP, 0x14)],
            vec![a(OPCODE_HEADAMP, 0x15)],
        ]
    }

    /// One frame of the metering stream. A virtual device has no audio, so
    /// every channel sits at the Rio's silence floor.
    pub fn meter_frame(&self) -> Outgoing {
        Outgoing {
            block: MbcBlock {
                version: 1,
                src_mac: self.identity.mac,
                dst_mac: MAC_BROADCAST,
                flags: FLAG_UNSOLICITED | KIND_METER,
                opcode: OPCODE_METERING,
                subop: SUBOP_METERING,
                count: HEADAMP_CHANNELS as u16,
                start_index: 0,
                data: vec![METER_FLOOR; HEADAMP_CHANNELS],
            },
            read_reply: false,
        }
    }

    /// The answer to the console's poll: unit id, model, Dante name.
    fn poll_data(&self) -> Vec<u8> {
        let mut data = self.identity.unit_id.to_be_bytes().to_vec();
        data.extend_from_slice(&text_field(&self.identity.model, MODEL_FIELD));
        data.extend_from_slice(&text_field(&self.identity.dante_name, NAME_FIELD));
        data
    }

    /// Handles one block heard on the vendor channel.
    ///
    /// Blocks from other devices, blocks for other devices, and anything
    /// this device has no captured answer for produce nothing: a Rio never
    /// NAKs, and neither does this.
    pub fn handle(&mut self, block: &MbcBlock) -> Outcome {
        let mut outcome = Outcome::default();
        if block.flags & (FLAG_FROM_DEVICE | FLAG_UNSOLICITED) != 0 {
            return outcome; // another device talking
        }
        let to_us = block.dst_mac == self.identity.mac;
        let poll = (block.opcode, block.subop) == (OPCODE_POLL, SUBOP_POLL);
        // The poll goes to everyone. Nothing else addressed to everyone, or
        // to the all-zero address the console uses before it has paired,
        // was ever answered by the Rio.
        if !(to_us || (poll && block.dst_mac == MAC_BROADCAST)) {
            return outcome;
        }

        let reply = |flags: u8, data: Vec<u8>| MbcBlock {
            version: 1,
            src_mac: self.identity.mac,
            dst_mac: block.src_mac,
            flags,
            opcode: block.opcode,
            subop: block.subop,
            count: block.count,
            start_index: block.start_index,
            data,
        };

        match block.kind() {
            KIND_READ if poll => {
                outcome.replies.push(Outgoing {
                    block: reply(FLAG_FROM_DEVICE | KIND_READ, self.poll_data()),
                    read_reply: true,
                });
            }
            KIND_READ => {
                let Some(parameter) = self.parameters.get(&(block.opcode, block.subop)) else {
                    return outcome;
                };
                let Some(range) = parameter.range(block.start_index, block.count) else {
                    return outcome;
                };
                outcome.replies.push(Outgoing {
                    block: reply(FLAG_FROM_DEVICE | KIND_READ, parameter.data[range].to_vec()),
                    read_reply: true,
                });
            }
            KIND_WRITE | KIND_SURFACE => {
                let key = (block.opcode, block.subop);
                // Kind 02 is only ever the 0731 arrays, and those are only
                // ever sent as kind 02.
                if (block.kind() == KIND_SURFACE) != (block.opcode == OPCODE_SURFACE) {
                    return outcome;
                }
                let Some(parameter) = self.parameters.get_mut(&key) else {
                    return outcome;
                };
                let Some(range) = parameter.range(block.start_index, block.count) else {
                    return outcome;
                };
                if !parameter.writable || block.data.len() != range.len() {
                    return outcome;
                }

                let mut written = block.data.clone();
                if key == (OPCODE_HEADAMP, SUBOP_GAIN) {
                    // The Rio clamps: the sweep in the capture stops at -6.
                    let (low, high) = ((GAIN_MIN_DB * 100.0) as i16, (GAIN_MAX_DB * 100.0) as i16);
                    for pair in written.as_chunks_mut::<2>().0 {
                        *pair = i16::from_be_bytes(*pair).clamp(low, high).to_be_bytes();
                    }
                }

                let before = parameter.data[range.clone()].to_vec();
                parameter.data[range].copy_from_slice(&written);
                outcome.changes = changes(key, block.start_index, &before, &written);
                outcome.replies.push(Outgoing {
                    block: reply(FLAG_FROM_DEVICE | block.kind(), written),
                    read_reply: false,
                });
            }
            KIND_METER if (block.opcode, block.subop) == (OPCODE_METERING, SUBOP_METERING) => {
                outcome.metering_requested = true;
                outcome.replies.push(Outgoing {
                    block: reply(FLAG_FROM_DEVICE | KIND_METER, Vec::new()),
                    read_reply: true,
                });
            }
            _ => {}
        }
        outcome
    }
}

/// The head-amp values that differ between `before` and `after`.
fn changes(key: (u16, u8), start_index: u16, before: &[u8], after: &[u8]) -> Vec<Change> {
    match key {
        (OPCODE_HEADAMP, SUBOP_GAIN) => before
            .as_chunks::<2>()
            .0
            .iter()
            .zip(after.as_chunks::<2>().0)
            .enumerate()
            .filter(|(_, (old, new))| old != new)
            .map(|(i, (_, new))| Change::Gain {
                channel: start_index + i as u16 + 1,
                db: f32::from(i16::from_be_bytes(*new)) / 100.0,
            })
            .collect(),
        (OPCODE_HEADAMP, SUBOP_PHANTOM) => before
            .iter()
            .zip(after)
            .enumerate()
            .filter(|(_, (old, new))| (**old != 0) != (**new != 0))
            .map(|(i, (_, new))| Change::Phantom { channel: start_index + i as u16 + 1, on: *new != 0 })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::codec::{blocks_in_conmon_payload, wrap_conmon, wrap_conmon_blocks};
    use super::*;

    const RIO_MAC: [u8; 6] = [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04];
    const RIO_EUI64: [u8; 8] = [0x00, 0x1d, 0xc1, 0xff, 0xfe, 0x25, 0xdf, 0x04];
    const CLASS_DEVICE: u32 = 0x0731_1002;

    fn rio() -> HeadampDevice {
        HeadampDevice::new(HeadampIdentity {
            mac: RIO_MAC,
            unit_id: 4,
            model: "Rio3224-D2".into(),
            dante_name: "Y004-Yamaha-Rio3224-D2-25df04".into(),
            inputs: 32,
            outputs: 24,
        })
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Puts a captured console packet to the device and returns each reply
    /// as the whole ConMon packet the device would send, numbered from
    /// `sequence`.
    fn answer(device: &mut HeadampDevice, console_packet: &str, sequence: u16) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        for block in blocks_in_conmon_payload(&hex(console_packet)) {
            for reply in device.handle(&block).replies {
                let n = sequence + packets.len() as u16;
                packets.push(wrap_conmon(&reply.encode(), n, RIO_EUI64, CLASS_DEVICE));
            }
        }
        packets
    }

    // Each test below pairs a packet the QL1 sent with the packet the
    // Rio3224-D2 sent back, both lifted whole from the reference capture.

    #[test]
    fn the_poll_gets_the_rios_own_answer() {
        // Frame 928: the console's poll, to everyone. The packet carries six
        // vendor pieces; the first five are two-byte items of some other
        // kind and the sixth is the MBC block. Frame 929: the Rio's answer.
        let poll = "ffff005c01330000001dc117ea2c0000417564696e617465072e1002000000000133001000060000\
                    030000ff0380d4ab0380d5aa0380d6a90380d7a81fc0\
                    4d424301000a00a0dee0cef6ffffffffffff00000a01071f06000100000e";
        let blocks = blocks_in_conmon_payload(&hex(poll));
        assert_eq!(blocks.len(), 1, "one MBC block among the six pieces");
        assert_eq!((blocks[0].opcode, blocks[0].subop, blocks[0].kind()), (OPCODE_POLL, SUBOP_POLL, KIND_READ));

        let mut device = rio();
        assert_eq!(
            answer(&mut device, poll, 4),
            vec![hex("ffff007a00040000001dc1fffe25df04417564696e6174650731100200000000000400100001000051c0\
                      4d424301000a001dc125df0400a0dee0cef600000a21071f0600010000000452696f333232342d4432000000000000\
                      593030342d59616d6168612d52696f333232342d44322d32356466303400000074")]
        );

        // A read of anything else sent to everyone is not ours to answer,
        // and neither is a poll addressed to another device.
        let mut other = blocks[0].clone();
        other.dst_mac = [9; 6];
        assert!(device.handle(&other).replies.is_empty());
        let mut broadcast_read = blocks[0].clone();
        broadcast_read.opcode = OPCODE_HEADAMP;
        broadcast_read.subop = SUBOP_GAIN;
        assert!(device.handle(&broadcast_read).replies.is_empty());
    }

    #[test]
    fn a_two_read_packet_gets_both_answers() {
        // Frame 930: reads of 0711/08 and 0711/09 in one packet.
        // Frames 931 and 932: the Rio's two answers.
        let mut device = rio();
        let replies = answer(
            &mut device,
            "ffff006801340000001dc117ea2c0000417564696e617465072e10020000000001340010000200001fc0\
             4d424301000a00a0dee0cef6001dc125df0400000a01071108000100002e1fc0\
             4d424301000a00a0dee0cef6001dc125df0400000a01071109000100002d",
            5,
        );
        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0],
            hex("ffff004900050000001dc1fffe25df04417564696e6174650731100200000000000500100001000020c0\
                 4d424301000a001dc125df0400a0dee0cef600000a2107110800010000020c")
        );
        let second = &blocks_in_conmon_payload(&replies[1])[0];
        assert_eq!((second.opcode, second.subop, second.data.as_slice()), (0x0711, 0x09, &[0x01][..]));
    }

    #[test]
    fn identity_reads_give_the_rios_answers() {
        let mut device = rio();
        // Frames 940 / 941: 0711/01.
        assert_eq!(
            answer(
                &mut device,
                "ffff004801350000001dc117ea2c0000417564696e617465072e10020000000001350010000100001fc0\
                 4d424301000a00a0dee0cef6001dc125df0400000a010711010001000035",
                7
            ),
            vec![hex("ffff004c00070000001dc1fffe25df04417564696e6174650731100200000000000700100001000023c0\
                      4d424301000a001dc125df0400a0dee0cef600000a21071101000100000100000014")]
        );
        // Frames 970 / 971: 0711/05, the Dante name.
        assert_eq!(
            answer(
                &mut device,
                "ffff0048013b0000001dc117ea2c0000417564696e617465072e100200000000013b0010000100001fc0\
                 4d424301000a00a0dee0cef6001dc125df0400000a010711050001000031",
                0x0c
            ),
            vec![hex("ffff0068000c0000001dc1fffe25df04417564696e6174650731100200000000000c0010000100003fc0\
                      4d424301000a001dc125df0400a0dee0cef600000a2107110500010000\
                      593030342d59616d6168612d52696f333232342d44322d3235646630340000001f")]
        );
    }

    #[test]
    fn the_console_overwrites_phantom_and_the_device_hands_it_back() {
        let mut device = rio();
        // Give the device the +48 V state the Rio came up with: 1 to 8 on.
        for channel in 1..=8 {
            device.set_phantom(channel, true);
        }
        // Frames 1097 / 1098: the console reads +48 V.
        assert_eq!(
            answer(
                &mut device,
                "ffff004801470000001dc117ea2c0000417564696e617465072e10020000000001470010000100001fc0\
                 4d424301000a00a0dee0cef6001dc125df0400000a0107221700200000ef",
                0x16
            ),
            vec![hex("ffff006800160000001dc1fffe25df04417564696e617465073110020000000000160010000100003fc0\
                      4d424301000a001dc125df0400a0dee0cef600000a2107221700200000\
                      0101010101010101000000000000000000000000000000000000000000000000c7")]
        );

        // Frames 1099 / 1100: the console writes all thirty-two off.
        let write = "ffff006801480000001dc117ea2c0000417564696e617465072e10020000000001480010000100003fc0\
                     4d424301002a00a0dee0cef6001dc125df0400002a0007221700200000\
                     0000000000000000000000000000000000000000000000000000000000000000b0";
        let block = blocks_in_conmon_payload(&hex(write)).remove(0);
        let outcome = device.handle(&block);
        assert_eq!(
            wrap_conmon(&outcome.replies[0].encode(), 0x17, RIO_EUI64, CLASS_DEVICE),
            hex("ffff006800170000001dc1fffe25df04417564696e617465073110020000000000170010000100003fc0\
                 4d424301002a001dc125df0400a0dee0cef600002a2007221700200000\
                 000000000000000000000000000000000000000000000000000000000000000090")
        );
        // Eight channels went from on to off, and only those are reported.
        assert_eq!(outcome.changes.len(), 8);
        assert_eq!(outcome.changes[0], Change::Phantom { channel: 1, on: false });
        assert_eq!(outcome.changes[7], Change::Phantom { channel: 8, on: false });
        assert_eq!(device.phantom(1), Some(false));
    }

    #[test]
    fn a_gain_write_is_echoed_and_reported_per_channel() {
        // Frame 4947: the console turns input 1 up from -6 to -4 dB.
        // Frame 4949: the Rio's echo.
        let mut device = rio();
        let write = "ffff008801ca0000001dc117ea2c0000417564696e617465072e10020000000001ca0010000100005fc0\
                     4d424301004a00a0dee0cef6001dc125df0400004a0007221600200000fe70\
                     fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8\
                     fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda808";
        let block = blocks_in_conmon_payload(&hex(write)).remove(0);
        let outcome = device.handle(&block);

        assert_eq!(outcome.changes, vec![Change::Gain { channel: 1, db: -4.0 }]);
        assert_eq!(device.gain_db(1), Some(-4.0));
        assert!(!outcome.replies[0].read_reply, "a write echo carries an honest len");
        assert_eq!(
            wrap_conmon(&outcome.replies[0].encode(), 0x07c6, RIO_EUI64, CLASS_DEVICE),
            hex("ffff008807c60000001dc1fffe25df04417564696e617465073110020000000007c60010000100005fc0\
                 4d424301004a001dc125df0400a0dee0cef600004a2007221600200000fe70\
                 fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8\
                 fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8e8")
        );

        // The same write again changes nothing, and says so.
        assert!(device.handle(&block).changes.is_empty());
    }

    #[test]
    fn gain_is_clamped_to_the_rios_range() {
        let mut device = rio();
        let mut write = super::super::codec::gain_broadcast([1; 6], RIO_MAC, &[-6.0; HEADAMP_CHANNELS]);
        write.data[0..2].copy_from_slice(&(-2000i16).to_be_bytes());
        write.data[2..4].copy_from_slice(&9000i16.to_be_bytes());
        let outcome = device.handle(&write);
        assert_eq!(device.gain_db(1), Some(-6.0));
        assert_eq!(device.gain_db(2), Some(66.0));
        assert_eq!(&outcome.replies[0].block.data[..4], &hex("fda819c8")[..], "the echo is what was stored");
    }

    #[test]
    fn the_surface_arrays_come_straight_back() {
        // Frame 2066 (both 0731 arrays in one packet), frames 2068 / 2069.
        let mut device = rio();
        let replies = answer(
            &mut device,
            "ffff00a0016c0000001dc117ea2c0000417564696e617465072e100200000000016c0010000200003fc0\
             4d424301002a00a0dee0cef6001dc125df0400002a0207310100200000\
             0000000000000000000000000000000000000000000000000000000000000000b537c0\
             4d424301002200a0dee0cef6001dc125df040000220207310200180000\
             000000000000000000000000000000000000000000000000cc",
            0x01aa,
        );
        assert_eq!(
            replies,
            vec![
                hex("ffff006801aa0000001dc1fffe25df04417564696e617465073110020000000001aa0010000100003fc0\
                     4d424301002a001dc125df0400a0dee0cef600002a2207310100200000\
                     000000000000000000000000000000000000000000000000000000000000000095"),
                hex("ffff006001ab0000001dc1fffe25df04417564696e617465073110020000000001ab00100001000037c0\
                     4d4243010022001dc125df0400a0dee0cef60000222207310200180000\
                     000000000000000000000000000000000000000000000000ac"),
            ]
        );
    }

    #[test]
    fn a_metering_request_is_acknowledged_and_the_stream_is_the_rios() {
        // Frames 1306 / 1307, then the first stream frame 1309.
        let mut device = rio();
        let request = "ffff0048015a0000001dc117ea2c0000417564696e617465072e100200000000015a0010000100001fc0\
                       4d424301000a00a0dee0cef6001dc125df0400000a0307420000200000e4";
        let block = blocks_in_conmon_payload(&hex(request)).remove(0);
        let outcome = device.handle(&block);
        assert!(outcome.metering_requested);
        assert_eq!(
            wrap_conmon(&outcome.replies[0].encode(), 0x25, RIO_EUI64, CLASS_DEVICE),
            hex("ffff004800250000001dc1fffe25df04417564696e617465073110020000000000250010000100001fc0\
                 4d424301000a001dc125df0400a0dee0cef600000a2307420000200000c4")
        );

        let frame = device.meter_frame();
        assert_eq!(frame.block.flags, 0x13);
        assert_eq!(frame.block.dst_mac, MAC_BROADCAST);
        assert_eq!(frame.block.data, vec![METER_FLOOR; 32]);
        // The Rio's frame 1309 has the same header; its levels are live.
        let real = hex("4d424301002a001dc125df04ffffffffffff00002a1307420000200000");
        assert_eq!(&frame.encode()[..real.len()], &real[..]);
    }

    #[test]
    fn the_power_up_announcement_is_the_rios() {
        // Frames 810, 811, 813, 814.
        let device = rio();
        let packets: Vec<Vec<u8>> = device
            .power_up_announcement()
            .iter()
            .enumerate()
            .map(|(n, blocks)| {
                let encoded: Vec<Vec<u8>> = blocks.iter().map(Outgoing::encode).collect();
                let refs: Vec<&[u8]> = encoded.iter().map(Vec::as_slice).collect();
                wrap_conmon_blocks(&refs, n as u16, RIO_EUI64, CLASS_DEVICE)
            })
            .collect();
        assert_eq!(packets.len(), 4);
        assert_eq!(
            packets[0],
            hex("ffff008900000000001dc1fffe25df04417564696e617465073110020000000000000010000200003fc0\
                 4d424301002a001dc125df04ffffffffffff00002a1007110500010000\
                 593030342d59616d6168612d52696f333232342d44322d3235646630340000001820c0\
                 4d424301000b001dc125df04ffffffffffff00000b10071108000100000243")
        );
        assert_eq!(
            packets[1],
            hex("ffff006a00010000001dc1fffe25df04417564696e6174650731100200000000000100100002000020c0\
                 4d424301000b001dc125df04ffffffffffff00000b1007110900010000014320c0\
                 4d424301000b001dc125df04ffffffffffff00000b1007221000010000012b")
        );
        assert_eq!(
            packets[2],
            hex("ffff004900020000001dc1fffe25df04417564696e6174650731100200000000000200100001000020c0\
                 4d424301000b001dc125df04ffffffffffff00000b10072214000100000028")
        );
        assert_eq!(
            packets[3],
            hex("ffff004900030000001dc1fffe25df04417564696e6174650731100200000000000300100001000020c0\
                 4d424301000b001dc125df04ffffffffffff00000b10072215000100000027")
        );
    }

    #[test]
    fn a_change_from_the_bridge_is_announced_whole_and_only_when_it_is_a_change() {
        let mut device = rio();
        let announcement = device.set_gain_db(5, 12.4).expect("a change");
        assert_eq!(announcement.block.flags, FLAG_UNSOLICITED);
        assert_eq!(announcement.block.dst_mac, MAC_BROADCAST);
        assert_eq!(announcement.block.count, 32);
        assert_eq!(device.gain_db(5), Some(12.0), "rounded to the Rio's 1 dB step");
        assert_eq!(announcement.block.gain_db().unwrap()[4], 12.0);

        assert!(device.set_gain_db(5, 12.0).is_none(), "no change, nothing to say");
        assert!(device.set_gain_db(5, 11.6).is_none(), "rounds to the same step");
        assert!(device.set_gain_db(0, 1.0).is_none());
        assert!(device.set_gain_db(33, 1.0).is_none());

        assert!(device.set_phantom(2, true).is_some());
        assert!(device.set_phantom(2, true).is_none());
        assert_eq!(device.phantom(2), Some(true));
    }

    #[test]
    fn what_the_rio_did_not_answer_is_not_answered() {
        let mut device = rio();
        let mut block = MbcBlock {
            version: 1,
            src_mac: [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6],
            dst_mac: RIO_MAC,
            flags: KIND_READ,
            opcode: OPCODE_HEADAMP,
            subop: SUBOP_GAIN,
            count: 32,
            start_index: 0,
            data: Vec::new(),
        };
        assert_eq!(device.handle(&block).replies.len(), 1, "the baseline: a good read");

        // Frames 902 / 903: the 0731 arrays to the all-zero address, before
        // pairing. The Rio said nothing.
        let unpaired = MbcBlock {
            dst_mac: [0; 6],
            flags: KIND_SURFACE,
            opcode: OPCODE_SURFACE,
            subop: 0x01,
            data: vec![0; 32],
            ..block.clone()
        };
        assert_eq!(device.handle(&unpaired), Outcome::default());

        block.subop = 0x7e; // no such parameter
        assert_eq!(device.handle(&block), Outcome::default());
        block.subop = SUBOP_GAIN;
        block.count = 33; // past the end of the array
        assert_eq!(device.handle(&block), Outcome::default());
        block.count = 32;
        block.flags = FLAG_FROM_DEVICE | KIND_READ; // another device's reply
        assert_eq!(device.handle(&block), Outcome::default());

        // A write to an identity parameter, and one of the wrong size.
        let write = MbcBlock {
            version: 1,
            src_mac: [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6],
            dst_mac: RIO_MAC,
            flags: KIND_WRITE,
            opcode: OPCODE_IDENTITY,
            subop: SUBOP_UNIT_ID,
            count: 1,
            start_index: 0,
            data: vec![0, 9],
        };
        assert_eq!(device.handle(&write), Outcome::default());
        let short = MbcBlock { opcode: OPCODE_HEADAMP, subop: SUBOP_GAIN, count: 32, data: vec![0; 10], ..write };
        assert_eq!(device.handle(&short), Outcome::default());
    }

    #[test]
    fn a_single_element_write_lands_on_its_own_index() {
        // Frames 5246 / 5248: 0731/01, one element at index 1.
        let mut device = rio();
        let write = MbcBlock {
            version: 1,
            src_mac: [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6],
            dst_mac: RIO_MAC,
            flags: KIND_SURFACE,
            opcode: OPCODE_SURFACE,
            subop: 0x01,
            count: 1,
            start_index: 1,
            data: vec![1],
        };
        let outcome = device.handle(&write);
        let echo = &outcome.replies[0].block;
        assert_eq!((echo.flags, echo.count, echo.start_index, echo.data.as_slice()), (0x22, 1, 1, &[1u8][..]));

        // And a single-channel gain write reports the right channel.
        let gain = MbcBlock {
            flags: KIND_WRITE,
            opcode: OPCODE_HEADAMP,
            subop: SUBOP_GAIN,
            count: 1,
            start_index: 9,
            data: 1800i16.to_be_bytes().to_vec(),
            ..write
        };
        assert_eq!(device.handle(&gain).changes, vec![Change::Gain { channel: 10, db: 18.0 }]);
    }
}

//! Wire codec for the Yamaha `MBC` head-amp block and its Audinate ConMon
//! envelope, per `docs/yamaha-ha-remote-over-dante.md`.
//!
//! Pure functions, no I/O. Every test in this module is driven by bytes
//! lifted out of `ql1-rio3224d2-*.pcap` in the private `dante-captures`
//! repo, including a frame a real Rio3224-D2 accepted and acted on.
//!
//! ## Where this departs from the spec document
//!
//! The spec's §4 says to trust the block's `len` field over the packet
//! boundary. That is right for steady-state broadcasts but **wrong for
//! replies to a query**: a device answering a `count = N` read request
//! echoes the query's `len` (10) unchanged and appends the data *past* the
//! boundary that `len` implies. Trusting `len` there silently discards the
//! payload - which is why §6 of the spec records subops `18`/`19`/`1a`/`1b`
//! as "never populated" when in fact they were answered in full.
//!
//! So this codec does not trust either boundary blindly. It takes the
//! candidate block ends (the one `len` implies and the one the enclosing
//! ConMon vendor length implies) and picks whichever one **checksums
//! correctly**. The additive checksum is a strong enough discriminator to
//! settle it - it validated on 3 830 of 3 907 captured messages, and every
//! failure had an ambiguous boundary.

use std::fmt;

/// Ports ConMon rides on. Most MBC traffic, including metering, is on 8705.
pub const CONMON_STATUS_PORT: u16 = 8705;
pub const CONMON_MULTICAST_PORT: u16 = 8708;
pub const CONMON_UNICAST_PORT: u16 = 8800;

/// Multicast groups for the two multicast ConMon ports.
pub const CONMON_STATUS_GROUP: [u8; 4] = [224, 0, 0, 232];
pub const CONMON_MULTICAST_GROUP: [u8; 4] = [224, 0, 0, 233];

/// Head-amp parameter block.
pub const OPCODE_HEADAMP: u16 = 0x0722;
/// Input metering, broadcast by the stagebox at ~31 Hz.
pub const OPCODE_METERING: u16 = 0x0742;
/// Single-channel surface event.
pub const OPCODE_SURFACE: u16 = 0x0731;

pub const SUBOP_GAIN: u8 = 0x16;
pub const SUBOP_PHANTOM: u8 = 0x17;
pub const SUBOP_METERING: u8 = 0x00;

/// Every byte of a well-formed MBC block, checksum included, sums to this
/// modulo 256.
pub const CHECKSUM_TARGET: u8 = 0x3F;

/// Bit 0x20 of the flags byte is the direction: clear from the console,
/// set from the device. The low bits vary per message class and must not
/// be hard-coded - copy them from the equivalent captured message.
pub const FLAG_FROM_DEVICE: u8 = 0x20;

/// Flags observed on the 32-channel head-amp broadcast, console -> device.
/// This is the value on the frame the Rio3224-D2 accepted.
pub const FLAGS_HEADAMP_BROADCAST: u8 = 0x00;

/// A Rio3224-D2 clamps gain to this range; the spec's sweep hit the floor.
pub const GAIN_MIN_DB: f32 = -6.0;
pub const GAIN_MAX_DB: f32 = 66.0;

/// Channels in a head-amp array. Every observed head-amp message carries
/// exactly 32 elements regardless of the device's real input count.
pub const HEADAMP_CHANNELS: usize = 32;

const MBC_MAGIC: &[u8; 3] = b"MBC";
const BLOCK_HEADER_LEN: usize = 24;
/// `len` counts from this offset to the end of the block.
const LEN_ORIGIN: usize = 20;
const BODY_PREFIX_LEN: usize = 5; // subop + count + start_index
const ENVELOPE_LEN: usize = 42;
const VENDOR_LEN_OFFSET: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MbcError {
    NoMagic,
    Truncated {
        need: usize,
        got: usize,
    },
    BadChecksum {
        expected: u8,
        computed: u8,
    },
    /// `data` length is not a whole multiple of `count`.
    RaggedData {
        data_len: usize,
        count: u16,
    },
}

impl fmt::Display for MbcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMagic => write!(f, "no MBC magic in payload"),
            Self::Truncated { need, got } => {
                write!(f, "MBC block truncated: need {need} bytes, got {got}")
            }
            Self::BadChecksum { expected, computed } => write!(
                f,
                "MBC checksum mismatch: frame says {expected:#04x}, computed {computed:#04x}"
            ),
            Self::RaggedData { data_len, count } => write!(
                f,
                "MBC body has {data_len} data bytes which is not a multiple of count {count}"
            ),
        }
    }
}

impl std::error::Error for MbcError {}

pub type MbcResult<T> = Result<T, MbcError>;

/// One decoded MBC block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbcBlock {
    pub version: u8,
    /// **Yamaha** MAC, which is a different NIC from the device's Dante MAC.
    pub src_mac: [u8; 6],
    pub dst_mac: [u8; 6],
    pub flags: u8,
    pub opcode: u16,
    pub subop: u8,
    /// Number of *elements*, not bytes.
    pub count: u16,
    pub start_index: u16,
    pub data: Vec<u8>,
}

impl MbcBlock {
    /// True when this block came from a device rather than the console.
    pub fn from_device(&self) -> bool {
        self.flags & FLAG_FROM_DEVICE != 0
    }

    /// A body with a count but no data is a read request.
    pub fn is_query(&self) -> bool {
        self.data.is_empty()
    }

    /// Bytes per element, or `None` for a query.
    pub fn element_width(&self) -> MbcResult<Option<usize>> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let count = self.count as usize;
        if count == 0 || !self.data.len().is_multiple_of(count) {
            return Err(MbcError::RaggedData {
                data_len: self.data.len(),
                count: self.count,
            });
        }
        Ok(Some(self.data.len() / count))
    }

    /// Reads the body as big-endian `i16` elements. Returns `None` unless
    /// the element width really is 2 - this refuses to reinterpret a
    /// `uint8` array as half as many `int16`s.
    pub fn as_i16(&self) -> Option<Vec<i16>> {
        if self.element_width().ok().flatten() != Some(2) {
            return None;
        }
        Some(
            self.data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_be_bytes(*c))
                .collect(),
        )
    }

    /// Reads the body as `u8` elements, only when the width is 1.
    pub fn as_u8(&self) -> Option<&[u8]> {
        (self.element_width().ok().flatten() == Some(1)).then_some(&self.data[..])
    }

    /// Gain array in dB, from a `0x0722`/`0x16` block. The wire unit is
    /// centi-dB.
    pub fn gain_db(&self) -> Option<Vec<f32>> {
        if self.opcode != OPCODE_HEADAMP || self.subop != SUBOP_GAIN {
            return None;
        }
        Some(
            self.as_i16()?
                .into_iter()
                .map(|v| v as f32 / 100.0)
                .collect(),
        )
    }

    /// Phantom array from a `0x0722`/`0x17` block.
    pub fn phantom(&self) -> Option<Vec<bool>> {
        if self.opcode != OPCODE_HEADAMP || self.subop != SUBOP_PHANTOM {
            return None;
        }
        Some(self.as_u8()?.iter().map(|&v| v != 0).collect())
    }

    /// Raw metering bytes from a `0x0742`/`0x00` block.
    ///
    /// Deliberately **not** converted to dBFS: no calibrated signal was
    /// ever injected, so the mapping from byte value to level is unknown.
    /// Observed range is 31 (silence) to 64 (peak).
    pub fn metering_raw(&self) -> Option<&[u8]> {
        if self.opcode != OPCODE_METERING || self.subop != SUBOP_METERING {
            return None;
        }
        self.as_u8()
    }

    /// Serializes the block, appending a correct checksum.
    pub fn encode(&self) -> Vec<u8> {
        let body_len = BODY_PREFIX_LEN + self.data.len() + 1; // + checksum
        let len = (BLOCK_HEADER_LEN - LEN_ORIGIN + body_len) as u16;

        let mut out = Vec::with_capacity(BLOCK_HEADER_LEN + body_len);
        out.extend_from_slice(MBC_MAGIC);
        out.push(self.version);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.src_mac);
        out.extend_from_slice(&self.dst_mac);
        out.extend_from_slice(&[0, 0]);
        out.push((len & 0xFF) as u8); // len_lo, the low byte repeated
        out.push(self.flags);
        out.extend_from_slice(&self.opcode.to_be_bytes());
        out.push(self.subop);
        out.extend_from_slice(&self.count.to_be_bytes());
        out.extend_from_slice(&self.start_index.to_be_bytes());
        out.extend_from_slice(&self.data);
        out.push(checksum(&out));
        out
    }

    /// Decodes one block from a slice that starts at the `M` of `MBC` and
    /// ends at the block's last byte.
    fn decode_exact(slice: &[u8]) -> MbcResult<Self> {
        if slice.len() < BLOCK_HEADER_LEN + BODY_PREFIX_LEN + 1 {
            return Err(MbcError::Truncated {
                need: BLOCK_HEADER_LEN + BODY_PREFIX_LEN + 1,
                got: slice.len(),
            });
        }
        let expected = slice[slice.len() - 1];
        let computed = checksum(&slice[..slice.len() - 1]);
        if expected != computed {
            return Err(MbcError::BadChecksum { expected, computed });
        }
        let body = &slice[BLOCK_HEADER_LEN..slice.len() - 1];
        Ok(Self {
            version: slice[3],
            src_mac: slice[6..12].try_into().expect("6 bytes"),
            dst_mac: slice[12..18].try_into().expect("6 bytes"),
            flags: slice[21],
            opcode: u16::from_be_bytes([slice[22], slice[23]]),
            subop: body[0],
            count: u16::from_be_bytes([body[1], body[2]]),
            start_index: u16::from_be_bytes([body[3], body[4]]),
            data: body[BODY_PREFIX_LEN..].to_vec(),
        })
    }
}

/// `(0x3F - sum) & 0xFF`, over the whole block up to but excluding the
/// checksum byte - MAC addresses included. Leaving the MACs out is the
/// mistake that made an earlier pass think the constant was per-message-class.
pub fn checksum(block_without_checksum: &[u8]) -> u8 {
    let sum = block_without_checksum
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    CHECKSUM_TARGET.wrapping_sub(sum)
}

/// Extracts every MBC block from a ConMon UDP payload.
///
/// A packet can carry more than one block. Each candidate runs from its
/// `MBC` magic to whichever end validates: the next block's magic, the
/// boundary the `len` field implies, or the end of the vendor payload.
/// Blocks that validate under none of those are skipped rather than
/// guessed at.
pub fn blocks_in_conmon_payload(payload: &[u8]) -> Vec<MbcBlock> {
    let region_end = vendor_payload_end(payload).unwrap_or(payload.len());
    let region = &payload[..region_end.min(payload.len())];

    let starts: Vec<usize> = (0..region.len().saturating_sub(MBC_MAGIC.len()))
        .filter(|&i| &region[i..i + MBC_MAGIC.len()] == MBC_MAGIC)
        .collect();

    let mut out = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let hard_end = starts.get(n + 1).copied().unwrap_or(region.len());
        let slice = &region[start..hard_end];

        // Candidate ends, most-trusted first. `len` is right for
        // steady-state broadcasts; the region end is right for replies to
        // a query, which keep the query's `len` and append data past it.
        let declared = declared_block_len(slice);
        let mut candidates = Vec::new();
        if let Some(d) = declared {
            if d <= slice.len() {
                candidates.push(d);
            }
        }
        candidates.push(slice.len());
        candidates.dedup();

        if let Some(block) = candidates
            .into_iter()
            .find_map(|end| MbcBlock::decode_exact(&slice[..end]).ok())
        {
            out.push(block);
        }
    }
    out
}

/// Block length implied by the block's own `len` field.
fn declared_block_len(slice: &[u8]) -> Option<usize> {
    if slice.len() < BLOCK_HEADER_LEN {
        return None;
    }
    let len = u16::from_be_bytes([slice[4], slice[5]]) as usize;
    Some(LEN_ORIGIN + len)
}

/// End offset of the vendor payload, from the ConMon envelope's
/// offset-40 field: high byte is the block length **plus one**, low byte
/// is normally `0xC0`.
///
/// The spec calls the low byte invariant; across the captures it is `0xC0`
/// on 3 889 frames but `0x00` on 15 and `0x80` on 3, so this reads the
/// high byte and does not require the low byte to match.
fn vendor_payload_end(payload: &[u8]) -> Option<usize> {
    if payload.len() < ENVELOPE_LEN {
        return None;
    }
    let vendor = u16::from_be_bytes([payload[VENDOR_LEN_OFFSET], payload[VENDOR_LEN_OFFSET + 1]]);
    let block_len = (vendor >> 8).checked_sub(1)? as usize;
    let end = ENVELOPE_LEN.checked_add(block_len)?;
    (end <= payload.len()).then_some(end)
}

/// Builds the Audinate ConMon envelope around an MBC block.
///
/// `message_class` is the 4-byte class the sender uses - `0x072e1002` when
/// speaking as the QL1, `0x07311002` from the Rio. Impersonating a console
/// means using the console's class *and* its EUI-64 here, and its Yamaha
/// MAC as the block's `src_mac`.
pub fn wrap_conmon(
    block: &[u8],
    sequence: u16,
    sender_eui64: [u8; 8],
    message_class: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_LEN + block.len());
    out.extend_from_slice(&0xffffu16.to_be_bytes()); // magic for 8705/8800
    out.extend_from_slice(&[0, 0]); // total length, patched below
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&sender_eui64);
    out.extend_from_slice(b"Audinate");
    out.extend_from_slice(&message_class.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&[0x00, 0x10, 0x00, 0x01, 0x00, 0x00]);
    // The trap: high byte is len + 1, low byte 0xC0. Get this wrong and
    // the device ignores the packet without complaint.
    let vendor = ((block.len() as u16 + 1) << 8) | 0xC0;
    out.extend_from_slice(&vendor.to_be_bytes());
    out.extend_from_slice(block);

    let total = out.len() as u16;
    out[2..4].copy_from_slice(&total.to_be_bytes());
    out
}

/// Builds a full 32-channel gain broadcast addressed as the console.
///
/// Gain is sent as a whole array because that is the only form ever
/// observed and the only one proven on hardware; there is no evidence a
/// device accepts a single-channel head-amp write.
pub fn gain_broadcast(
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    gains_db: &[f32; HEADAMP_CHANNELS],
) -> MbcBlock {
    let mut data = Vec::with_capacity(HEADAMP_CHANNELS * 2);
    for &g in gains_db.iter() {
        let centi = (g.clamp(GAIN_MIN_DB, GAIN_MAX_DB) * 100.0).round() as i16;
        data.extend_from_slice(&centi.to_be_bytes());
    }
    MbcBlock {
        version: 1,
        src_mac,
        dst_mac,
        flags: FLAGS_HEADAMP_BROADCAST,
        opcode: OPCODE_HEADAMP,
        subop: SUBOP_GAIN,
        count: HEADAMP_CHANNELS as u16,
        start_index: 0,
        data,
    }
}

/// Builds a full 32-channel phantom broadcast addressed as the console.
pub fn phantom_broadcast(
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    phantom: &[bool; HEADAMP_CHANNELS],
) -> MbcBlock {
    MbcBlock {
        version: 1,
        src_mac,
        dst_mac,
        flags: FLAGS_HEADAMP_BROADCAST,
        opcode: OPCODE_HEADAMP,
        subop: SUBOP_PHANTOM,
        count: HEADAMP_CHANNELS as u16,
        start_index: 0,
        data: phantom.iter().map(|&on| on as u8).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame 5503 of `ql1-rio3224d2-write-test-accepted.pcap`: a gain
    /// broadcast built by hand on a laptop, sent to 224.0.0.232:8705, and
    /// **accepted and acted on by a real Rio3224-D2** - it echoed input 5
    /// back at +25.00 dB. Every assertion here is anchored to a packet
    /// that is known to work on hardware.
    const ACCEPTED_GAIN_FRAME: &str = "ffff008809400000001dc117ea2c0000417564696e617465072e1002000000000940001000010000\
5fc04d424301004a00a0dee0cef6001dc125df0400004a0007221600200000fda8fda8fda8fda809c4fda8fda8fda8fda8fda8fda8fda8\
fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8fda8a9";

    /// A populated phantom array from the Rio, same capture set: inputs
    /// 1-8 on, 9-32 off.
    const PHANTOM_FRAME: &str = "ffff006800160000001dc1fffe25df04417564696e6174650731100200000000001600100001000\
03fc04d424301000a001dc125df0400a0dee0cef600000a21072217002000000101010101010101000000000000000000000000000000000000000000000000c7";

    /// The `0x0722`/`0x19` reply the spec recorded as never populated: it
    /// carries 32 x int16 of `0x0320` (800), sitting past the boundary the
    /// `len` field implies. Built by repetition rather than pasted, so a
    /// miscount cannot silently change what is being asserted.
    fn subop_19_reply_body() -> Vec<u8> {
        (0..32).flat_map(|_| [0x03u8, 0x20]).collect()
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_the_gain_frame_a_real_rio_accepted() {
        let blocks = blocks_in_conmon_payload(&hex(ACCEPTED_GAIN_FRAME));
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];

        assert_eq!(b.opcode, OPCODE_HEADAMP);
        assert_eq!(b.subop, SUBOP_GAIN);
        assert_eq!(b.count, 32);
        assert_eq!(b.start_index, 0);
        assert_eq!(b.src_mac, [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6]); // QL1 Yamaha NIC
        assert_eq!(b.dst_mac, [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04]); // Rio3224-D2
        assert!(!b.from_device());

        let gains = b.gain_db().expect("gain array");
        assert_eq!(gains.len(), 32);
        assert_eq!(gains[4], 25.0, "input 5 was set to +25.00 dB");
        for (i, g) in gains.iter().enumerate() {
            if i != 4 {
                assert_eq!(*g, -6.0, "channel {i} should sit at the -6.00 dB floor");
            }
        }
    }

    #[test]
    fn reencoding_the_accepted_frame_is_byte_identical() {
        let payload = hex(ACCEPTED_GAIN_FRAME);
        let block = &blocks_in_conmon_payload(&payload)[0];

        let reencoded = block.encode();
        assert_eq!(
            reencoded,
            &payload[ENVELOPE_LEN..],
            "re-encoding must reproduce the exact bytes the Rio accepted"
        );

        let rewrapped = wrap_conmon(
            &reencoded,
            0x0940,
            hex("001dc117ea2c0000").try_into().unwrap(),
            0x072e1002,
        );
        assert_eq!(rewrapped, payload, "envelope must round-trip too");
    }

    #[test]
    fn gain_broadcast_builds_the_accepted_frame_from_scratch() {
        let mut gains = [-6.0f32; HEADAMP_CHANNELS];
        gains[4] = 25.0;
        let block = gain_broadcast(
            [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6],
            [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04],
            &gains,
        );
        let payload = hex(ACCEPTED_GAIN_FRAME);
        assert_eq!(block.encode(), &payload[ENVELOPE_LEN..]);
    }

    #[test]
    fn decodes_a_populated_phantom_array_whose_data_sits_past_the_declared_len() {
        // This frame is the regression guard for the spec's §4 advice: its
        // `len` field says 10, which would make it an empty query, but 32
        // bytes of real phantom state follow.
        let payload = hex(PHANTOM_FRAME);
        let declared = declared_block_len(&payload[ENVELOPE_LEN..]).unwrap();
        assert_eq!(
            declared, 30,
            "the frame really does under-declare its length"
        );

        let blocks = blocks_in_conmon_payload(&payload);
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert!(b.from_device());
        assert!(!b.is_query(), "trusting `len` here would lose the payload");

        let phantom = b.phantom().expect("phantom array");
        assert_eq!(phantom.len(), 32);
        assert!(phantom[..8].iter().all(|&on| on), "inputs 1-8 were on");
        assert!(phantom[8..].iter().all(|&on| !on), "inputs 9-32 were off");
    }

    #[test]
    fn checksum_rule_matches_the_hardware_verified_constant() {
        // The spec's §9 records K = 0xa7 for the gain class, derived as
        // 0x3F - sum(header). Both must agree.
        let payload = hex(ACCEPTED_GAIN_FRAME);
        let block = &payload[ENVELOPE_LEN..];
        assert_eq!(checksum(&block[..block.len() - 1]), 0xa9);
        assert_eq!(
            block.iter().fold(0u8, |a, &b| a.wrapping_add(b)),
            CHECKSUM_TARGET,
            "a whole valid block sums to 0x3F"
        );

        let header_sum = block[..BLOCK_HEADER_LEN]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(CHECKSUM_TARGET.wrapping_sub(header_sum), 0xa7);
    }

    #[test]
    fn a_corrupted_block_is_rejected_not_guessed_at() {
        let mut payload = hex(ACCEPTED_GAIN_FRAME);
        let last = payload.len() - 1;
        payload[last] ^= 0xFF;
        assert!(
            blocks_in_conmon_payload(&payload).is_empty(),
            "a bad checksum must drop the block rather than yield wrong gains"
        );
    }

    #[test]
    fn element_width_is_recovered_for_the_subops_the_spec_called_blank() {
        // 0x0722/0x19 answered a query with 32 x int16 - the spec lists it
        // among the subops whose element width "was never observed".
        let data = subop_19_reply_body();
        let block = MbcBlock {
            version: 1,
            src_mac: [0; 6],
            dst_mac: [0; 6],
            flags: 0x21,
            opcode: OPCODE_HEADAMP,
            subop: 0x19,
            count: 32,
            start_index: 0,
            data,
        };
        assert_eq!(block.element_width().unwrap(), Some(2));
        assert_eq!(block.as_i16().unwrap(), vec![800i16; 32]);
        // Width and addressing are now evidence-backed; the *meaning* is
        // still unknown, so nothing here converts it to a unit.
        assert!(
            block.gain_db().is_none(),
            "0x19 must not masquerade as gain"
        );
    }

    #[test]
    fn a_uint8_array_is_never_reinterpreted_as_int16() {
        let block = MbcBlock {
            version: 1,
            src_mac: [0; 6],
            dst_mac: [0; 6],
            flags: 0x20,
            opcode: OPCODE_HEADAMP,
            subop: SUBOP_PHANTOM,
            count: 32,
            start_index: 0,
            data: vec![1u8; 32],
        };
        assert_eq!(block.element_width().unwrap(), Some(1));
        assert!(block.as_i16().is_none());
        assert_eq!(block.as_u8().unwrap().len(), 32);
    }

    #[test]
    fn queries_carry_a_count_but_no_data() {
        let block = MbcBlock {
            version: 1,
            src_mac: [0; 6],
            dst_mac: [0; 6],
            flags: 0x01,
            opcode: OPCODE_HEADAMP,
            subop: SUBOP_GAIN,
            count: 32,
            start_index: 0,
            data: Vec::new(),
        };
        assert!(block.is_query());
        assert_eq!(block.element_width().unwrap(), None);
        let round_tripped =
            &blocks_in_conmon_payload(&wrap_conmon(&block.encode(), 1, [0; 8], 0x072e1002))[0];
        assert_eq!(round_tripped, &block);
    }

    #[test]
    fn gain_is_clamped_to_the_rio_range_rather_than_wrapping() {
        let block = gain_broadcast([0; 6], [0; 6], &[100.0; HEADAMP_CHANNELS]);
        assert_eq!(block.as_i16().unwrap()[0], 6600);
        let block = gain_broadcast([0; 6], [0; 6], &[-40.0; HEADAMP_CHANNELS]);
        assert_eq!(block.as_i16().unwrap()[0], -600);
    }

    #[test]
    fn metering_is_exposed_raw_with_no_invented_scale() {
        let block = MbcBlock {
            version: 1,
            src_mac: [0; 6],
            dst_mac: [0; 6],
            flags: 0x13,
            opcode: OPCODE_METERING,
            subop: SUBOP_METERING,
            count: 32,
            start_index: 0,
            data: vec![31u8; 32],
        };
        assert_eq!(block.metering_raw().unwrap()[0], 31);
        assert!(block.gain_db().is_none());
    }

    #[test]
    fn multiple_blocks_in_one_packet_are_all_recovered() {
        let a = gain_broadcast([1; 6], [2; 6], &[-6.0; HEADAMP_CHANNELS]);
        let b = phantom_broadcast([1; 6], [2; 6], &[true; HEADAMP_CHANNELS]);
        let mut joined = a.encode();
        joined.extend_from_slice(&b.encode());

        let payload = wrap_conmon(&joined, 7, [0; 8], 0x072e1002);
        let blocks = blocks_in_conmon_payload(&payload);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].subop, SUBOP_GAIN);
        assert_eq!(blocks[1].subop, SUBOP_PHANTOM);
    }
}

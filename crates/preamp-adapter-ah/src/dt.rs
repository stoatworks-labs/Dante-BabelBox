//! Allen & Heath **DT168 / DT164-W** Dante-expander preamp control — the
//! `AllenHth` ConMon vendor protocol.
//!
//! **Protocol source:** static reverse-engineering of A&H's shipped
//! *DT Preamp Control V1.21* app and the SQ Dante-card (KLANTE) firmware,
//! written up in [`docs/allenheath-dt-preamp-over-dante.md`](../../../docs/allenheath-dt-preamp-over-dante.md).
//! Read that first — it tags every claim HIGH / MED / OPEN. Nothing here was
//! captured from the wire or proven against hardware.
//!
//! ## Codec only — there is no transport
//!
//! A&H tunnels this inside Audinate **ConMon** vendor messages (vendor ID the
//! 8-byte ASCII `AllenHth`). Delivering those needs the Audinate DAPI, which
//! this workspace deliberately does not link — the same reason the Aphex
//! adapter ships as a codec with no transport. So this module **builds and
//! parses the A&H vendor payload** (the bytes *inside* the ConMon envelope)
//! and stops there. When a ConMon transport exists (a capture-anchored raw
//! implementation, or DAPI), it wraps [`encode_set`]/[`encode_poll`] and feeds
//! [`decode_status`] — and a `DeviceAdapter` becomes a thin wrapper, exactly
//! like [`crate::dlive`].
//!
//! ## What is solid vs. provisional
//!
//! Solid (HIGH, seen in two artefacts): vendor ID `AllenHth`; 16 preamps per
//! device; state is `{ u16 gain, bool pad, bool phantom, name[32] }`; the full
//! status message is 16×`[u16 gain][u8 flags]`.
//!
//! Provisional (MED/OPEN): the `u16` gain **byte order** and its **UWORD→dB
//! scale**, and the exact **field offsets of the 16-byte set message**. These
//! are marked at each use and gathered in the doc's "What would settle each
//! [OPEN]" table. The gain↔dB helpers are flagged `provisional_` precisely so a
//! caller can't mistake them for verified.

/// A&H's ConMon vendor ID: the 8-byte ASCII `AllenHth`. Confirmed in both the
/// app's send path and the KLANTE card firmware headers. [HIGH]
pub const VENDOR_ID: [u8; 8] = *b"AllenHth";

/// Number of mic preamps a DT168 / DT164-W exposes. [HIGH]
pub const PREAMP_COUNT: usize = 16;

/// The UWORD gain reference the app uses (its self-test sets gain `0x8000`, and
/// the QML centres the driver here). [HIGH for the value, OPEN for its meaning]
pub const GAIN_REFERENCE: u16 = 0x8000;

/// ConMon wire-envelope constants, verified from a real Dante capture (a DVS
/// broadcasting as vendor `Audinate`); see the "ConMon wire envelope" section of
/// `docs/allenheath-dt-preamp-over-dante.md`. The envelope is vendor-independent
/// — an `AllenHth` mic-pre message rides in the identical frame.
pub mod conmon {
    /// magic on the status/monitoring channel.
    pub const MAGIC_STATUS: u16 = 0xFFFE;
    /// magic on the control channel.
    pub const MAGIC_CONTROL: u16 = 0xFFFF;
    /// Multicast group + UDP port for status/metering broadcasts (`vendor_broadcast`).
    pub const STATUS_GROUP: [u8; 4] = [224, 0, 0, 233];
    pub const STATUS_PORT: u16 = 8708;
    /// Multicast group + UDP port for control/device-info.
    pub const CONTROL_GROUP: [u8; 4] = [224, 0, 0, 231];
    pub const CONTROL_PORT: u16 = 8702;
}

/// A parsed ConMon wire envelope: everything around the vendor `body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConmonFrame {
    pub magic: u16,
    pub seqnum: u16,
    pub device_id: [u8; 8],
    pub vendor: [u8; 8],
    pub body: Vec<u8>,
}

/// Build a device's 8-byte EUI-64 ConMon id from its 6-byte MAC (FF:FE inserted
/// mid-6), as seen on the wire.
pub fn device_eui64(mac: [u8; 6]) -> [u8; 8] {
    [mac[0], mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5]]
}

/// Wrap a vendor `body` in the ConMon envelope. `vendor` is the 8-byte id
/// ([`VENDOR_ID`] for A&H). The `length` field is the whole message.
pub fn wrap_conmon(magic: u16, seqnum: u16, device_id: [u8; 8], vendor: [u8; 8], body: &[u8]) -> Vec<u8> {
    let total = 24 + body.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&magic.to_be_bytes());
    out.extend_from_slice(&(total as u16).to_be_bytes());
    out.extend_from_slice(&seqnum.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&device_id);
    out.extend_from_slice(&vendor);
    out.extend_from_slice(body);
    out
}

/// Parse a ConMon envelope, returning the header fields and the vendor `body`.
/// `None` if too short to hold the 24-byte header.
pub fn parse_conmon(pkt: &[u8]) -> Option<ConmonFrame> {
    if pkt.len() < 24 {
        return None;
    }
    Some(ConmonFrame {
        magic: u16::from_be_bytes([pkt[0], pkt[1]]),
        seqnum: u16::from_be_bytes([pkt[4], pkt[5]]),
        device_id: pkt[8..16].try_into().ok()?,
        vendor: pkt[16..24].try_into().ok()?,
        body: pkt[24..].to_vec(),
    })
}

/// A&H vendor message types (7-entry dispatch table in the app). Only the three
/// below are identified; the numbering is [MED].
pub mod msg_type {
    /// Controller → device: set one preamp's gain/pad/phantom.
    pub const SET_MIC_PRE: u8 = 0x05;
    /// Device → controller: full 16-preamp status (payload selector `0x01`).
    pub const STATUS_FULL: u8 = 0x01;
    /// Device → controller: flags-only status (payload selector `0x00`).
    pub const STATUS_FLAGS: u8 = 0x00;
}

/// Byte order of the on-wire `u16` gain. **[OPEN]** — the app reads it with a
/// native-endian load and there is no capture to disambiguate. Big-endian is
/// the working assumption (it matches A&H's DDP fields and normal network
/// order); flip this one constant if a capture says otherwise.
const GAIN_IS_BIG_ENDIAN: bool = true;

fn gain_to_wire(g: u16) -> [u8; 2] {
    if GAIN_IS_BIG_ENDIAN {
        g.to_be_bytes()
    } else {
        g.to_le_bytes()
    }
}

fn gain_from_wire(b: [u8; 2]) -> u16 {
    if GAIN_IS_BIG_ENDIAN {
        u16::from_be_bytes(b)
    } else {
        u16::from_le_bytes(b)
    }
}

/// Bit positions in the status `flags` byte. `_SetStatusForMicPre` masks bit 0
/// and bit 1 as the two booleans. **[MED]** which is which — pad is taken as
/// bit 0 and phantom as bit 1 (flip [`FLAG_PAD`]/[`FLAG_PHANTOM`] if a capture
/// disagrees).
pub const FLAG_PAD: u8 = 1 << 0;
pub const FLAG_PHANTOM: u8 = 1 << 1;

/// One preamp's control state, at the wire level (raw `u16` gain, not dB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DtPreamp {
    pub gain: u16,
    pub pad: bool,
    pub phantom: bool,
}

impl DtPreamp {
    fn flags_byte(&self) -> u8 {
        let mut f = 0;
        if self.pad {
            f |= FLAG_PAD;
        }
        if self.phantom {
            f |= FLAG_PHANTOM;
        }
        f
    }

    fn from_flags(gain: u16, flags: u8) -> Self {
        Self {
            gain,
            pad: flags & FLAG_PAD != 0,
            phantom: flags & FLAG_PHANTOM != 0,
        }
    }
}

/// Errors decoding an A&H DT vendor payload.
#[derive(Debug, PartialEq, Eq)]
pub enum DtDecodeError {
    /// Payload too short for the claimed message shape.
    Truncated { need: usize, got: usize },
    /// Selector byte was neither `STATUS_FULL` nor `STATUS_FLAGS`.
    UnknownStatusSelector(u8),
}

impl core::fmt::Display for DtDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DtDecodeError::Truncated { need, got } => {
                write!(f, "DT status payload truncated: need {need} bytes, got {got}")
            }
            DtDecodeError::UnknownStatusSelector(s) => {
                write!(f, "DT status has unknown selector 0x{s:02x}")
            }
        }
    }
}

impl std::error::Error for DtDecodeError {}

/// Build the **A&H vendor payload** for a "set mic-pre" on one preamp.
///
/// This is the payload that goes *inside* the ConMon vendor envelope, not a
/// full ConMon message. **[OPEN]** the exact field offsets of the real 16-byte
/// message were not pinned to the byte; this lays them out as
/// `[type][channel][gain_hi][gain_lo][flags]` — the fields the app is known to
/// pack — so a capture can correct the ordering without changing the model.
///
/// `channel` is 0-based (`0..16`).
pub fn encode_set(channel: u8, preamp: DtPreamp) -> Vec<u8> {
    let g = gain_to_wire(preamp.gain);
    vec![
        msg_type::SET_MIC_PRE,
        channel,
        g[0],
        g[1],
        preamp.flags_byte(),
    ]
}

/// Build the A&H vendor payload that requests a full status dump
/// (`_Mess_GetAllMicPre`). **[MED]** — a bare "get all" with no per-channel
/// fields is all the app is seen to send.
pub fn encode_poll() -> Vec<u8> {
    vec![msg_type::STATUS_FULL]
}

/// Parse a device → controller **status** payload into the 16 preamp states.
///
/// Selector `0x01` (full) carries `[u16 gain][u8 flags]` per preamp; selector
/// `0x00` (flags-only) carries just the flags and leaves gain unknown, returned
/// as [`GAIN_REFERENCE`]. `payload[0]` is the selector; entries follow. **[MED]**
pub fn decode_status(payload: &[u8]) -> Result<Vec<DtPreamp>, DtDecodeError> {
    let selector = *payload.first().ok_or(DtDecodeError::Truncated { need: 1, got: 0 })?;
    let body = &payload[1..];
    match selector {
        msg_type::STATUS_FULL => {
            let need = PREAMP_COUNT * 3;
            if body.len() < need {
                return Err(DtDecodeError::Truncated {
                    need: need + 1,
                    got: payload.len(),
                });
            }
            Ok(body
                .as_chunks::<3>()
                .0
                .iter()
                .take(PREAMP_COUNT)
                .map(|c| DtPreamp::from_flags(gain_from_wire([c[0], c[1]]), c[2]))
                .collect())
        }
        msg_type::STATUS_FLAGS => {
            let need = PREAMP_COUNT;
            if body.len() < need {
                return Err(DtDecodeError::Truncated {
                    need: need + 1,
                    got: payload.len(),
                });
            }
            Ok(body
                .iter()
                .take(PREAMP_COUNT)
                .map(|&flags| DtPreamp::from_flags(GAIN_REFERENCE, flags))
                .collect())
        }
        other => Err(DtDecodeError::UnknownStatusSelector(other)),
    }
}

/// Parse a firmware-version payload (`major.minor.patch` at payload bytes
/// 4/5/6). **[MED]** Returns `None` if too short.
pub fn decode_firmware_version(payload: &[u8]) -> Option<(u8, u8, u8)> {
    Some((*payload.get(4)?, *payload.get(5)?, *payload.get(6)?))
}

// ---------------------------------------------------------------------------
// Provisional gain <-> dB. UNVERIFIED: the real scale lives in A&H's AHDrivers
// layer and was not reversed. These exist only so a caller that must surface a
// dB number has one clearly-labelled place to do it — and one place to fix when
// a capture pins the range. Do not present their output as accurate.
// ---------------------------------------------------------------------------

/// Provisional linear gain range, in dB. **UNVERIFIED** placeholder — the app
/// takes min/max from the device at runtime and the UWORD→dB curve is unknown.
pub const PROVISIONAL_GAIN_MIN_DB: f32 = 0.0;
/// See [`PROVISIONAL_GAIN_MIN_DB`]. **UNVERIFIED** placeholder.
pub const PROVISIONAL_GAIN_MAX_DB: f32 = 60.0;

/// Map a raw UWORD gain to dB with the provisional linear range. **UNVERIFIED.**
pub fn provisional_gain_to_db(uword: u16) -> f32 {
    let frac = uword as f32 / u16::MAX as f32;
    PROVISIONAL_GAIN_MIN_DB + frac * (PROVISIONAL_GAIN_MAX_DB - PROVISIONAL_GAIN_MIN_DB)
}

/// Inverse of [`provisional_gain_to_db`]. **UNVERIFIED.**
pub fn provisional_db_to_gain(db: f32) -> u16 {
    let span = PROVISIONAL_GAIN_MAX_DB - PROVISIONAL_GAIN_MIN_DB;
    let frac = ((db - PROVISIONAL_GAIN_MIN_DB) / span).clamp(0.0, 1.0);
    (frac * u16::MAX as f32).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_id_is_allenhth() {
        assert_eq!(&VENDOR_ID, b"AllenHth");
        assert_eq!(VENDOR_ID.len(), 8);
    }

    #[test]
    fn set_encodes_type_channel_gain_flags() {
        let msg = encode_set(3, DtPreamp { gain: 0x8000, pad: false, phantom: true });
        assert_eq!(msg[0], msg_type::SET_MIC_PRE);
        assert_eq!(msg[1], 3);
        assert_eq!([msg[2], msg[3]], 0x8000u16.to_be_bytes());
        assert_eq!(msg[4], FLAG_PHANTOM);
    }

    #[test]
    fn flags_byte_packs_pad_and_phantom_independently() {
        assert_eq!(DtPreamp { gain: 0, pad: false, phantom: false }.flags_byte(), 0);
        assert_eq!(DtPreamp { gain: 0, pad: true, phantom: false }.flags_byte(), FLAG_PAD);
        assert_eq!(DtPreamp { gain: 0, pad: false, phantom: true }.flags_byte(), FLAG_PHANTOM);
        assert_eq!(
            DtPreamp { gain: 0, pad: true, phantom: true }.flags_byte(),
            FLAG_PAD | FLAG_PHANTOM
        );
    }

    #[test]
    fn full_status_decodes_sixteen_preamps() {
        let mut payload = vec![msg_type::STATUS_FULL];
        for i in 0..PREAMP_COUNT as u16 {
            let gain = 0x1000 + i;
            payload.extend_from_slice(&gain.to_be_bytes());
            // even preamps: pad on; multiples of 3: phantom on
            let mut flags = 0;
            if i % 2 == 0 {
                flags |= FLAG_PAD;
            }
            if i % 3 == 0 {
                flags |= FLAG_PHANTOM;
            }
            payload.push(flags);
        }
        let decoded = decode_status(&payload).unwrap();
        assert_eq!(decoded.len(), PREAMP_COUNT);
        assert_eq!(decoded[0], DtPreamp { gain: 0x1000, pad: true, phantom: true });
        assert_eq!(decoded[1], DtPreamp { gain: 0x1001, pad: false, phantom: false });
        assert_eq!(decoded[15].gain, 0x100F);
    }

    #[test]
    fn set_then_decode_round_trips_one_preamp() {
        let pre = DtPreamp { gain: 0xABCD, pad: true, phantom: false };
        // Splice the encoded fields into a single-entry full-status body and
        // read them back — proves gain byte order and flag bits are consistent
        // between encode and decode.
        let set = encode_set(0, pre);
        let mut want = vec![msg_type::STATUS_FULL, set[2], set[3], set[4]];
        // pad the remaining 15 entries so the length check passes
        want.extend(std::iter::repeat_n(0u8, (PREAMP_COUNT - 1) * 3));
        let decoded = decode_status(&want).unwrap();
        assert_eq!(decoded[0], pre);
    }

    #[test]
    fn flags_only_status_uses_reference_gain() {
        let mut payload = vec![msg_type::STATUS_FLAGS];
        payload.extend(std::iter::repeat_n(FLAG_PAD, PREAMP_COUNT));
        let decoded = decode_status(&payload).unwrap();
        assert_eq!(decoded.len(), PREAMP_COUNT);
        assert!(decoded.iter().all(|p| p.gain == GAIN_REFERENCE && p.pad && !p.phantom));
    }

    #[test]
    fn truncated_status_is_rejected_not_panicked() {
        assert_eq!(decode_status(&[]), Err(DtDecodeError::Truncated { need: 1, got: 0 }));
        let short = vec![msg_type::STATUS_FULL, 0x00, 0x01];
        assert!(matches!(decode_status(&short), Err(DtDecodeError::Truncated { .. })));
    }

    #[test]
    fn unknown_selector_is_reported() {
        assert_eq!(
            decode_status(&[0x7f, 0, 0, 0]),
            Err(DtDecodeError::UnknownStatusSelector(0x7f))
        );
    }

    #[test]
    fn firmware_version_reads_bytes_4_5_6() {
        let payload = vec![0, 0, 0, 0, 1, 6, 3];
        assert_eq!(decode_firmware_version(&payload), Some((1, 6, 3)));
        assert_eq!(decode_firmware_version(&[0, 0, 0]), None);
    }

    #[test]
    fn parses_a_real_dvs_conmon_header() {
        // First 24 bytes of an actual 84-byte status frame captured from a DVS
        // on 224.0.0.233:8708, plus a couple body bytes.
        let pkt = bytes(
            "fffe 0054 0000 0000 36ec26fffe9c7240 4175 64696e617465 0800",
        );
        let f = parse_conmon(&pkt).unwrap();
        assert_eq!(f.magic, conmon::MAGIC_STATUS);
        assert_eq!(f.device_id, bytes("36ec26fffe9c7240")[..]);
        assert_eq!(&f.vendor, b"Audinate");
        assert_eq!(f.body, bytes("0800"));
    }

    #[test]
    fn wrap_then_parse_round_trips_an_allenhth_body() {
        let dev = device_eui64([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        assert_eq!(dev, bytes("001122fffe334455")[..]);
        let body = encode_set(2, DtPreamp { gain: 0x4000, pad: true, phantom: false });
        let pkt = wrap_conmon(conmon::MAGIC_STATUS, 7, dev, VENDOR_ID, &body);
        assert_eq!(u16::from_be_bytes([pkt[2], pkt[3]]) as usize, pkt.len());
        let f = parse_conmon(&pkt).unwrap();
        assert_eq!(f.seqnum, 7);
        assert_eq!(&f.vendor, b"AllenHth");
        assert_eq!(f.body, body);
    }

    fn bytes(hex: &str) -> Vec<u8> {
        let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn provisional_db_round_trips_within_a_step() {
        for db in [0.0f32, 5.0, 30.0, 45.0, 60.0] {
            let back = provisional_gain_to_db(provisional_db_to_gain(db));
            assert!((back - db).abs() < 0.01, "{db} -> {back}");
        }
    }
}

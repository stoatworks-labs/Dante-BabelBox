//! ANSI E1.17 (ACN) PDU walking and DMP property decoding, per
//! `docs/SHURE-ACN.md` in RFutils.
//!
//! Verified against real receiver traffic in `dante-captures`
//! (`qlxd4-*.pcap`), and cross-checked against Wireshark's own `acn`
//! dissector. Where the hand decode and Wireshark disagreed on vector
//! *names*, Wireshark's numbering is authoritative - `Ack` is 14, not 1.

/// Both source and destination port for SDT sessions.
pub const SDT_PORT: u16 = 57383;
/// Where the console multicasts to; inside the E1.17 SDT range.
pub const SDT_MULTICAST_GROUP: [u8; 4] = [239, 195, 234, 61];
pub const SDT_MULTICAST_PORT: u16 = 5568;

const PREAMBLE: &[u8] = b"\x00\x10\x00\x00ASC-E1.17\x00\x00\x00";

/// DMP rides inside SDT client blocks under this protocol id.
pub const PROTOCOL_DMP: u32 = 0x0000_0002;

pub const SDT_VECTOR_UNRELIABLE_WRAPPER: u8 = 2;
pub const SDT_VECTOR_RELIABLE_WRAPPER: u8 = 1;

pub const DMP_VECTOR_GET_PROPERTY_REPLY: u8 = 3;
pub const DMP_VECTOR_EVENT: u8 = 4;
pub const DMP_VECTOR_SET_PROPERTY: u8 = 2;

/// Address header for a single, absolute, non-virtual 4-byte address -
/// the only form these receivers use.
const ADDR_HEADER_4BYTE_ABSOLUTE: u8 = 0x02;

/// One PDU in an ACN block, with vector and header already resolved
/// through the inheritance rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdu<'a> {
    pub vector: &'a [u8],
    pub header: &'a [u8],
    pub data: &'a [u8],
}

/// Walks a sequence of ACN PDUs.
///
/// The flags nibble says which parts are present: `0x4` vector, `0x2`
/// header, `0x1` data, `0x8` a 3-byte length instead of 12 bits. Anything
/// absent is **inherited from the previous PDU** in the same block, which
/// is why this cannot be parsed one PDU at a time in isolation.
///
/// `vector_len` and `header_len` are fixed by the enclosing layer, so the
/// caller supplies them.
pub fn walk_pdus(buf: &[u8], vector_len: usize, header_len: usize) -> Vec<Pdu<'_>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut last_vector: &[u8] = &[];
    let mut last_header: &[u8] = &[];

    while offset + 2 <= buf.len() {
        let flags = buf[offset] >> 4;
        let (length, header_bytes) = if flags & 0x8 != 0 {
            if offset + 3 > buf.len() {
                break;
            }
            let len = (((buf[offset] & 0x0F) as usize) << 16)
                | ((buf[offset + 1] as usize) << 8)
                | buf[offset + 2] as usize;
            (len, 3)
        } else {
            let len = (((buf[offset] & 0x0F) as usize) << 8) | buf[offset + 1] as usize;
            (len, 2)
        };

        if length < header_bytes || offset + length > buf.len() {
            break;
        }
        let body = &buf[offset + header_bytes..offset + length];
        let mut cursor = 0usize;

        let vector = if flags & 0x4 != 0 {
            if cursor + vector_len > body.len() {
                break;
            }
            cursor += vector_len;
            &body[cursor - vector_len..cursor]
        } else {
            last_vector
        };

        let header = if flags & 0x2 != 0 {
            if cursor + header_len > body.len() {
                break;
            }
            cursor += header_len;
            &body[cursor - header_len..cursor]
        } else {
            last_header
        };

        let data = if flags & 0x1 != 0 {
            &body[cursor..]
        } else {
            &[][..]
        };

        last_vector = vector;
        last_header = header;
        out.push(Pdu {
            vector,
            header,
            data,
        });
        offset += length;
    }
    out
}

/// A property value, sized by this crate's own table rather than by the
/// wire - DMP values are not self-describing.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int8(i8),
    UInt8(u8),
    Int16(i16),
    UInt32(u32),
    Int32(i32),
    Text(String),
}

/// What a property means and how wide it is.
///
/// The **only** reason this table has to exist is that DMP values carry no
/// type information: both ends are supposed to learn widths from the
/// device's DDL, and these receivers advertise a DDL over TFTP that they
/// do not actually serve (verified - not even an ERROR packet comes back).
/// So an address this table does not know cannot be skipped safely, and
/// parsing stops there rather than sliding out of alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Int8,
    UInt8,
    Int16,
    UInt32,
    Int32,
    /// Length-prefixed: a `u16` total length that **includes its own two
    /// bytes**, then unterminated characters.
    Text,
}

pub const PROP_MODEL_NAME: u32 = 0x0100_0000;
pub const PROP_DEVICE_NAME: u32 = 0x0100_0012;
pub const PROP_CHANNEL_NAME: u32 = 0x0200_0001;
pub const PROP_AF_OUTPUT_LEVEL: u32 = 0x0200_0102;
pub const PROP_FREQUENCY_KHZ: u32 = 0x0200_0804;
pub const PROP_RF_LEVEL_DBM: u32 = 0x0200_0114;
pub const PROP_RF_BARS: u32 = 0x0200_0101;
pub const PROP_BATTERY_BARS: u32 = 0x0200_1100;
/// Unresolved. Swings roughly -50..-102 with a carrier, but keeps drifting
/// between about -65 and -55 with **no carrier at all**, so it is not an
/// audio meter. Decoded, never interpreted, and given no unit.
pub const PROP_UNRESOLVED_LEVEL: u32 = 0x0200_0812;
/// Unresolved 0-2 indicator that tracks `PROP_UNRESOLVED_LEVEL`.
pub const PROP_UNRESOLVED_INDICATOR: u32 = 0x0200_0815;
/// Subscribed but never once emitted, holding -1 across every capture
/// including a full battery swap. Matches the sentinel Command Strings
/// return for `BATT_RUN_TIME` when unavailable; plausible, unconfirmed.
pub const PROP_BATTERY_RUN_TIME: u32 = 0x0200_110a;
/// As above, matching `BATT_CHARGE`'s unavailable sentinel.
pub const PROP_BATTERY_CHARGE: u32 = 0x0200_1126;
pub const PROP_UNKNOWN_0104: u32 = 0x0200_0104;

/// Sentinel meaning "no recent data" - **not** "no carrier". When a
/// carrier drops the value holds its last reading and only falls to -1
/// after a longer dropout.
pub const NO_RECENT_DATA: i8 = -1;

/// The RF level reported with no carrier present. A floor, not a
/// measurement.
pub const RF_FLOOR_DBM: i32 = -50;

pub fn property_kind(address: u32) -> Option<PropertyKind> {
    Some(match address {
        PROP_MODEL_NAME | PROP_DEVICE_NAME | PROP_CHANNEL_NAME => PropertyKind::Text,
        PROP_AF_OUTPUT_LEVEL | PROP_BATTERY_BARS | PROP_BATTERY_CHARGE => PropertyKind::Int8,
        PROP_RF_BARS | PROP_UNRESOLVED_INDICATOR | PROP_UNKNOWN_0104 => PropertyKind::UInt8,
        PROP_UNRESOLVED_LEVEL | PROP_BATTERY_RUN_TIME => PropertyKind::Int16,
        PROP_FREQUENCY_KHZ => PropertyKind::UInt32,
        PROP_RF_LEVEL_DBM => PropertyKind::Int32,
        _ => return None,
    })
}

/// One decoded property report.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyReport {
    pub address: u32,
    pub value: Value,
}

/// Decodes the property carried by one DMP PDU.
///
/// Returns `None` for an address this crate has no width for, or a
/// truncated value - never a guess.
pub fn decode_property(header: &[u8], data: &[u8]) -> Option<PropertyReport> {
    if header.first()? != &ADDR_HEADER_4BYTE_ABSOLUTE || data.len() < 4 {
        return None;
    }
    let address = u32::from_be_bytes(data[..4].try_into().ok()?);
    let raw = &data[4..];
    let value = match property_kind(address)? {
        PropertyKind::Int8 => Value::Int8(*raw.first()? as i8),
        PropertyKind::UInt8 => Value::UInt8(*raw.first()?),
        PropertyKind::Int16 => Value::Int16(i16::from_be_bytes(raw.get(..2)?.try_into().ok()?)),
        PropertyKind::UInt32 => Value::UInt32(u32::from_be_bytes(raw.get(..4)?.try_into().ok()?)),
        PropertyKind::Int32 => Value::Int32(i32::from_be_bytes(raw.get(..4)?.try_into().ok()?)),
        PropertyKind::Text => {
            let total = u16::from_be_bytes(raw.get(..2)?.try_into().ok()?) as usize;
            let text = raw.get(2..total.max(2))?;
            Value::Text(String::from_utf8_lossy(text).into_owned())
        }
    };
    Some(PropertyReport { address, value })
}

/// Pulls every property report out of a whole SDT/DMP datagram.
///
/// Skips anything that is not a DMP client block carrying an `EVENT`,
/// `GET_PROPERTY_REPLY` or `SET_PROPERTY`.
pub fn properties_in_datagram(datagram: &[u8]) -> Vec<PropertyReport> {
    let Some(rest) = datagram.strip_prefix(PREAMBLE) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // Root layer: 4-byte vector, 16-byte header (the source CID).
    for root in walk_pdus(rest, 4, 16) {
        // SDT layer: 1-byte vector, no header.
        for sdt in walk_pdus(root.data, 1, 0) {
            let Some(&vector) = sdt.vector.first() else {
                continue;
            };
            if vector != SDT_VECTOR_UNRELIABLE_WRAPPER && vector != SDT_VECTOR_RELIABLE_WRAPPER {
                continue;
            }
            // Wrapper header is 20 bytes: channel, tseq, rseq, oldest,
            // first-MAK, last-MAK, MAK-threshold. Client blocks follow.
            const WRAPPER_HEADER: usize = 20;
            if sdt.data.len() < WRAPPER_HEADER {
                continue;
            }
            // Client blocks: 2-byte vector (member id), 6-byte header
            // (protocol id + association).
            for block in walk_pdus(&sdt.data[WRAPPER_HEADER..], 2, 6) {
                if block.header.len() < 4
                    || u32::from_be_bytes(block.header[..4].try_into().unwrap()) != PROTOCOL_DMP
                {
                    continue;
                }
                // DMP layer: 1-byte vector, 1-byte address header.
                for dmp in walk_pdus(block.data, 1, 1) {
                    let Some(&vector) = dmp.vector.first() else {
                        continue;
                    };
                    if !matches!(
                        vector,
                        DMP_VECTOR_EVENT | DMP_VECTOR_GET_PROPERTY_REPLY | DMP_VECTOR_SET_PROPERTY
                    ) {
                        continue;
                    }
                    if let Some(report) = decode_property(dmp.header, dmp.data) {
                        out.push(report);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `EVENT` datagram from the QLXD4 in
    /// `qlxd4-battery-swap.pcap`, receiver -> console, with **no carrier
    /// present**: RF pinned at the -50 dBm floor and 0 RF bars.
    const REAL_EVENT: &str = "001000004153432d45312e3137000000705f00000001dd47e0d7000011dda000000eddcccccc\
704902f17100001ee80000005c0000005cffffffff00007032ffff000000020000700904020200081502700c040202000114ffffffce\
700904020200010100700a040202000812ffc9";

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_a_real_event_from_the_receiver() {
        let props = properties_in_datagram(&hex(REAL_EVENT));
        assert_eq!(props.len(), 4, "this frame carries four properties");

        assert_eq!(
            props[0],
            PropertyReport {
                address: PROP_UNRESOLVED_INDICATOR,
                value: Value::UInt8(2)
            }
        );
        assert_eq!(
            props[1],
            PropertyReport {
                address: PROP_RF_LEVEL_DBM,
                value: Value::Int32(-50)
            },
            "the no-carrier floor, exactly as the spec records it"
        );
        assert_eq!(
            props[2],
            PropertyReport {
                address: PROP_RF_BARS,
                value: Value::UInt8(0)
            },
            "0 bars with no transmitter on"
        );
        assert_eq!(
            props[3],
            PropertyReport {
                address: PROP_UNRESOLVED_LEVEL,
                value: Value::Int16(-55)
            },
            "drifting near -55 with no carrier - which is why it is not audio"
        );
    }

    #[test]
    fn a_datagram_without_the_acn_preamble_is_ignored() {
        assert!(properties_in_datagram(b"not an acn packet at all").is_empty());
    }

    #[test]
    fn an_unknown_address_stops_decoding_rather_than_misaligning() {
        // Width is unknowable for an address outside the table, so the
        // property is dropped instead of consuming an arbitrary number of
        // bytes and corrupting everything after it.
        let header = [ADDR_HEADER_4BYTE_ABSOLUTE];
        let data = [0x09, 0x99, 0x99, 0x99, 0x42];
        assert!(decode_property(&header, &data).is_none());
    }

    #[test]
    fn text_properties_use_the_inclusive_length_prefix() {
        // "0009 House 1" - 7 characters plus the two length bytes.
        let mut data = PROP_DEVICE_NAME.to_be_bytes().to_vec();
        data.extend_from_slice(&9u16.to_be_bytes());
        data.extend_from_slice(b"House 1");
        let report = decode_property(&[ADDR_HEADER_4BYTE_ABSOLUTE], &data).unwrap();
        assert_eq!(report.value, Value::Text("House 1".into()));
    }

    #[test]
    fn battery_bars_carry_a_no_data_sentinel_distinct_from_empty() {
        let mut data = PROP_BATTERY_BARS.to_be_bytes().to_vec();
        data.push(0xFF); // -1
        let report = decode_property(&[ADDR_HEADER_4BYTE_ABSOLUTE], &data).unwrap();
        assert_eq!(report.value, Value::Int8(NO_RECENT_DATA));

        let mut data = PROP_BATTERY_BARS.to_be_bytes().to_vec();
        data.push(0); // a real zero-bar reading
        let report = decode_property(&[ADDR_HEADER_4BYTE_ABSOLUTE], &data).unwrap();
        assert_eq!(report.value, Value::Int8(0));
    }

    #[test]
    fn pdu_inheritance_reuses_the_previous_vector_and_header() {
        // Two PDUs: the first declares vector and header, the second sets
        // only the data flag and must inherit both.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x70, 0x06, 0xAA, 0xBB, 0x01]); // flags 7, len 6
        buf.push(0x99);
        buf.extend_from_slice(&[0x10, 0x03, 0x77]); // flags 1, len 3, data only

        let pdus = walk_pdus(&buf, 2, 1);
        assert_eq!(pdus.len(), 2);
        assert_eq!(pdus[0].vector, &[0xAA, 0xBB]);
        assert_eq!(pdus[0].header, &[0x01]);
        assert_eq!(pdus[1].vector, &[0xAA, 0xBB], "vector is inherited");
        assert_eq!(pdus[1].header, &[0x01], "header is inherited");
        assert_eq!(pdus[1].data, &[0x77]);
    }

    #[test]
    fn a_truncated_pdu_is_dropped_rather_than_read_past_the_buffer() {
        let buf = [0x70, 0x40, 0x01, 0x02];
        assert!(walk_pdus(&buf, 2, 1).is_empty());
    }
}

//! The ConMon status messages a Dante device publishes about itself, and the
//! queries that ask for them.
//!
//! A controller sends a short query to a device's control port (UDP 8700);
//! the device answers by publishing the matching status message on the
//! status group (224.0.0.231:8702), and unicasts a copy to every controller
//! that registered for the status channel (see [`crate::cmc`]).
//!
//! ## Where the bytes come from
//!
//! Each body below is what a real Rio3224-D2 (Brooklyn 3 module) sent in
//! `ql1-rio3224d2-pairing-gain-phantom.control.pcap`, with the device's own
//! MAC, device id and address patched to the virtual device's. Most fields
//! in these messages are **not decoded**: they are Audinate's, they describe
//! the Dante module rather than the product, and they are sent as captured.
//! What is known is named in each builder. The tests rebuild the captured
//! frames from the Rio's own identity, so a slip in an offset fails a test
//! rather than going out on the wire.
//!
//! The one message built from fields is make-and-model
//! ([`make_model`]), because that is where a persona differs.

use crate::conmon::{self, Header};
use crate::identity::DeviceIdentity;
use crate::persona::Persona;
use crate::hexutil::hex;

/// Software version a Brooklyn 3 puts on its status messages.
pub const VERSION_STATUS: u16 = 0x0734;
/// ...and on the make-and-model message and vendor messages. Two processes
/// on the module, two versions.
pub const VERSION_PRODUCT: u16 = 0x0731;

pub const TYPE_INTERFACE: u16 = 0x0011;
pub const TYPE_SWITCH: u16 = 0x0014;
pub const TYPE_CLOCK: u16 = 0x0020;
pub const TYPE_VERSIONS: u16 = 0x0060;
pub const TYPE_SAMPLE_RATE: u16 = 0x0080;
pub const TYPE_ENCODING: u16 = 0x0082;
pub const TYPE_0084: u16 = 0x0084;
pub const TYPE_MAKE_MODEL: u16 = 0x00c0;
pub const TYPE_1007: u16 = 0x1007;
pub const TYPE_1009: u16 = 0x1009;

/// The status type a control-port query asks for, by the query's own type.
///
/// Read off the capture: the QL1 sent each query on the left and the Rio
/// published the status on the right within a few milliseconds. The pairing
/// is not a formula (`0x0013` asks for `0x0011`), so it is a table.
pub fn status_for_query(query_type: u16) -> Option<u16> {
    Some(match query_type {
        0x0013 => TYPE_INTERFACE,
        // Dante Controller's, not the console's: seen answered this way by
        // a DM3's module. The body sent is the Rio's own, from its start-up.
        0x0015 => TYPE_SWITCH,
        0x0021 => TYPE_CLOCK,
        0x0061 => TYPE_VERSIONS,
        0x0081 => TYPE_SAMPLE_RATE,
        0x0083 => TYPE_ENCODING,
        0x0085 => TYPE_0084,
        0x00c1 => TYPE_MAKE_MODEL,
        0x1006 => TYPE_1007,
        0x1008 => TYPE_1009,
        _ => return None,
    })
}

/// What a device publishes unprompted when it comes up, in the Rio's order.
pub const BOOT_ANNOUNCE: [u16; 10] = [
    TYPE_1009,
    TYPE_CLOCK,
    TYPE_VERSIONS,
    TYPE_SAMPLE_RATE,
    TYPE_INTERFACE,
    TYPE_0084,
    TYPE_ENCODING,
    TYPE_MAKE_MODEL,
    TYPE_1007,
    TYPE_SWITCH,
];

/// What a device keeps repeating. The Rio republished its clock status every
/// two to four seconds and the rest far less often; a controller that joins
/// late still learns the device without having to ask.
pub const PERIODIC: [u16; 4] = [TYPE_CLOCK, TYPE_SAMPLE_RATE, TYPE_ENCODING, TYPE_MAKE_MODEL];

// Bodies as captured from the Rio3224-D2. Offsets are into the body, which
// starts at byte 28 of the packet.

/// Frame 560. The device's clock (PTP) status at power-up, before it has
/// found a leader: its own MAC stands in all three clock-id slots.
///
/// A virtual device runs no PTP, so this is the honest thing to say: it
/// names itself and no leader. Dante Controller will show it as not synced.
const CLOCK: &str = "00000000000100060000007b00000000001dc125df040000001dc125df040000001dc125df0400000001003400020000\
        02b4000000030d4000000002000000000000000000000000000000000008000100600c000000000c00a8002000040000\
        006810000000000101020100000000020002000700010002020201000000000200030007000100030102020000000002\
        00030003000100040202020000000002000300030004000700c80004001dc1fffe25df04001dc1fffe25df04001dc1ff\
        fe25df0400010000000100000001000000010000";
const CLOCK_MAC_AT: [usize; 3] = [16, 24, 32];
const CLOCK_EUI_AT: [usize; 3] = [172, 180, 188];

/// Frame 592. Module and firmware versions ("Bklyn3", "Brooklyn-III").
const VERSIONS: &str = "00000000040200050402000000000000426b6c796e3300000e78f6db000003e800000000000000030000001c04020005\
        00000001000003000000000042726f6f6b6c796e2d494949000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b\
        0000382100000007000000070000000000000000";

/// Frame 596. Current sample rate (48000 at offset 8) and the rates the
/// module supports (44100, 48000, 88200, 96000).
const SAMPLE_RATE: &str = "00000000001800040000bb8000000000000200000000ac440000bb800001588800017700";
const SAMPLE_RATE_AT: usize = 8;

/// Frame 601. Sample encoding: the current width in bits at offset 8, and
/// at the end the two the module supports (24 and 32).
const ENCODING: &str = "00000000001800020000001800000000000200000000001800000020";
const ENCODING_AT: usize = 8;
/// The width a Brooklyn 3 comes up with.
pub const DEFAULT_ENCODING_BITS: u16 = 24;

/// Frame 600.
const STATUS_0084: &str = "000000000030000500000000000000000002000000000001000000000000000000000000000000000000000000000000\
        00000001000000020000000300000004";

/// Frame 700.
const STATUS_1007: &str = "0000000000000000";

/// Frame 558.
const STATUS_1009: &str = "0000000000000001000000100000000000000000";

/// Frame 624. The module's switch configuration: the two names in it are
/// its modes, "Switched" and "Redundant". Sent as captured.
const SWITCH: &str = "0000000000020018009400040000007f0001000100010000537769746368656400000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000002b00000000000000000000000000020000526564756e64616e740000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000000000000029000000220000000000000000";

/// Frame 599. The network interface: link speed (1000), MAC, address and
/// netmask.
const INTERFACE: &str = "0000000000010001000003e80001001dc125df04a9fec1e5ffff00000000000000000000001800300000000000000000\
        0000000000000000000000000000000000000000";
const INTERFACE_MAC_AT: usize = 14;
const INTERFACE_IP_AT: usize = 20;

/// Frame 557. The once-a-second monitoring heartbeat; the counter is at
/// offset 12.
const HEARTBEAT: &str = "1000000000108001000400040004000000000000";
const HEARTBEAT_COUNTER_AT: usize = 12;

fn patch(body: &mut [u8], at: usize, bytes: &[u8]) {
    body[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The new encoding width, if a control-port message of type `0x0083` is a
/// *write* and not a query.
///
/// Both use the same type. A query's body opens `000f4240`; the one write in
/// the capture (the QL1 moving the Rio from 24 to 32 bit) opened with zero
/// and carried `0000 0001 | 0000 0020`. The Rio then published its encoding
/// status with the new width.
pub fn encoding_write(query_type: u16, body: &[u8]) -> Option<u16> {
    if query_type != 0x0083 || body.len() < 12 || body[0..4] != [0, 0, 0, 0] {
        return None;
    }
    let bits = u32::from_be_bytes(body[8..12].try_into().ok()?);
    matches!(bits, 16 | 24 | 32).then_some(bits as u16)
}

/// Builds the body of status message `message_type`, or `None` for a type
/// this crate has no captured reference for. `encoding_bits` is the
/// device's current sample width, which a controller can change.
pub fn body(
    message_type: u16,
    identity: &DeviceIdentity,
    persona: &Persona,
    encoding_bits: u16,
) -> Option<Vec<u8>> {
    Some(match message_type {
        TYPE_CLOCK => {
            let mut b = hex(CLOCK);
            for at in CLOCK_MAC_AT {
                patch(&mut b, at, &identity.mac);
            }
            for at in CLOCK_EUI_AT {
                patch(&mut b, at, &identity.eui64());
            }
            b
        }
        TYPE_VERSIONS => hex(VERSIONS),
        TYPE_SAMPLE_RATE => {
            let mut b = hex(SAMPLE_RATE);
            patch(&mut b, SAMPLE_RATE_AT, &persona.sample_rate.to_be_bytes());
            b
        }
        TYPE_ENCODING => {
            let mut b = hex(ENCODING);
            patch(&mut b, ENCODING_AT, &u32::from(encoding_bits).to_be_bytes());
            b
        }
        TYPE_0084 => hex(STATUS_0084),
        TYPE_1007 => hex(STATUS_1007),
        TYPE_1009 => hex(STATUS_1009),
        TYPE_SWITCH => hex(SWITCH),
        TYPE_INTERFACE => {
            let mut b = hex(INTERFACE);
            patch(&mut b, INTERFACE_MAC_AT, &identity.mac);
            patch(&mut b, INTERFACE_IP_AT, &identity.ip.octets());
            b
        }
        TYPE_MAKE_MODEL => make_model(persona),
        _ => return None,
    })
}

/// The software version a Brooklyn 3 stamps on `message_type`.
pub fn version_for(message_type: u16) -> u16 {
    if message_type == TYPE_MAKE_MODEL {
        VERSION_PRODUCT
    } else {
        VERSION_STATUS
    }
}

/// A complete status message, ready to send.
pub fn message(
    message_type: u16,
    sequence: u16,
    identity: &DeviceIdentity,
    persona: &Persona,
    encoding_bits: u16,
) -> Option<Vec<u8>> {
    let body = body(message_type, identity, persona, encoding_bits)?;
    let header = Header {
        magic: conmon::MAGIC,
        sequence,
        process: 0,
        device_id: identity.eui64(),
        vendor: conmon::VENDOR_AUDINATE,
        class: (u32::from(version_for(message_type)) << 16) | u32::from(message_type),
    };
    Some(conmon::encode(&header, &body))
}

/// The monitoring heartbeat (224.0.0.233:8708), `counter` counting seconds.
pub fn heartbeat(sequence: u16, counter: u16, identity: &DeviceIdentity) -> Vec<u8> {
    let mut body = hex(HEARTBEAT);
    patch(&mut body, HEARTBEAT_COUNTER_AT, &counter.to_be_bytes());
    let header = Header {
        magic: conmon::MAGIC_MONITOR,
        sequence,
        process: 0,
        device_id: identity.eui64(),
        vendor: conmon::VENDOR_AUDINATE,
        class: 0x0008_0001,
    };
    conmon::encode(&header, &body)
}

/// Make and model (`0x00c0`), the one status built from fields:
///
/// ```text
/// offset  size  field
///      0     4  zero
///      4     8  maker id, ASCII, zero padded          "Yamaha"
///     12     8  Audinate model id                     "8R19"
///     20     8  zero
///     28    12  three words, sent as captured (see below)
///     40     8  zero
///     48   128  maker's name                          "Yamaha Corporation"
///    176   128  product name                          "Rio3224-D2"
///    304     4  product version: major, minor, 0, 0   01 55 00 00
///    308    32  product version as text               "V1.85"
/// ```
fn make_model(persona: &Persona) -> Vec<u8> {
    fn field(out: &mut Vec<u8>, text: &str, width: usize) {
        let bytes = text.as_bytes();
        let take = bytes.len().min(width);
        out.extend_from_slice(&bytes[..take]);
        out.resize(out.len() + (width - take), 0);
    }

    let mut b = Vec::with_capacity(340);
    b.extend_from_slice(&[0; 4]);
    field(&mut b, &persona.manufacturer_id, 8);
    field(&mut b, &persona.model_id, 8);
    b.extend_from_slice(&[0; 8]);
    // Not decoded. The first two read like version numbers (5.0.0.1,
    // 5.0.0.2). The third was 0x1015 for the Rio's first second and 0x1017
    // from then on, so it holds a status bit of some kind; it is sent in
    // the settled form, which is what the console was looking at when it
    // paired.
    b.extend_from_slice(&hex("050000010500000200001017"));
    b.extend_from_slice(&[0; 8]);
    field(&mut b, &persona.manufacturer_name, 128);
    field(&mut b, &persona.model_name, 128);
    b.extend_from_slice(&[persona.product_version.0, persona.product_version.1, 0, 0]);
    field(&mut b, &persona.product_version_text(), 32);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rio() -> (DeviceIdentity, Persona) {
        (
            DeviceIdentity {
                name: "Y004-Yamaha-Rio3224-D2-25df04".into(),
                mac: [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04],
                ip: "169.254.193.229".parse().unwrap(),
            },
            Persona::yamaha_rio3224_d2(),
        )
    }

    /// Frame 1051, whole: the Rio's make-and-model status, as it answered
    /// the console's query during pairing.
    const RIO_MAKE_MODEL_FRAME: &str = "ffff017000370000001dc1fffe25df04417564696e617465073100c0\
        0000000059616d6168610000385231390000000000000000000000000500000105000002000010170000000000000000\
        59616d61686120436f72706f726174696f6e000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000052696f333232342d4432000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\
        000000000000000000000000000000000155000056312e38350000000000000000000000000000000000000000000000\
        00000000";

    /// Frame 599, whole: the Rio's interface status.
    const RIO_INTERFACE_FRAME: &str = "ffff006000140000001dc1fffe25df04417564696e61746507340011\
        0000000000010001000003e80001001dc125df04a9fec1e5ffff00000000000000000000001800300000000000000000\
        0000000000000000000000000000000000000000";

    /// Frame 557, whole: the Rio's first heartbeat.
    const RIO_HEARTBEAT_FRAME: &str =
        "fffe003000070000001dc1fffe25df04417564696e617465000800011000000000108001000400040004000000000000";

    #[test]
    fn make_model_rebuilds_the_captured_frame_from_the_persona() {
        let (identity, persona) = rio();
        let built = message(TYPE_MAKE_MODEL, 0x0037, &identity, &persona, 24).unwrap();
        assert_eq!(built, hex(RIO_MAKE_MODEL_FRAME));
    }

    #[test]
    fn interface_status_rebuilds_the_captured_frame() {
        let (identity, persona) = rio();
        let built = message(TYPE_INTERFACE, 0x0014, &identity, &persona, 24).unwrap();
        assert_eq!(built, hex(RIO_INTERFACE_FRAME));
    }

    #[test]
    fn switch_status_is_the_rios_own_344_byte_frame() {
        let (identity, persona) = rio();
        let built = message(TYPE_SWITCH, 0x001b, &identity, &persona, 24).unwrap();
        assert_eq!(built.len(), 344);
        assert_eq!(&built[..32], &hex("ffff0158001b0000001dc1fffe25df04417564696e6174650734001400000000")[..]);
        assert_eq!(&built[52..60], b"Switched");
        assert_eq!(&built[200..209], b"Redundant");
    }

    #[test]
    fn heartbeat_rebuilds_the_captured_frame() {
        let (identity, _) = rio();
        assert_eq!(heartbeat(0x0007, 4, &identity), hex(RIO_HEARTBEAT_FRAME));
    }

    #[test]
    fn every_identity_field_in_a_template_is_patched() {
        // Build each status as a different device and check nothing of the
        // Rio's identity is left in it. A missed offset would send a real
        // device's MAC out under another device's name.
        let (rio_identity, persona) = rio();
        let other = DeviceIdentity {
            name: "Y005-Yamaha-Rio3224-D2-a1b2c3".into(),
            mac: [0x02, 0xbb, 0x5a, 0xa1, 0xb2, 0xc3],
            ip: "169.254.10.20".parse().unwrap(),
        };
        let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);

        for message_type in BOOT_ANNOUNCE {
            let body = body(message_type, &other, &persona, 24).unwrap();
            assert!(
                !contains(&body, &rio_identity.mac),
                "status {message_type:#06x} still carries the reference MAC"
            );
            assert!(
                !contains(&body, &rio_identity.eui64()),
                "status {message_type:#06x} still carries the reference device id"
            );
            assert!(
                !contains(&body, &rio_identity.ip.octets()),
                "status {message_type:#06x} still carries the reference address"
            );
        }

        let clock = body(TYPE_CLOCK, &other, &persona, 24).unwrap();
        assert_eq!(clock.windows(6).filter(|w| *w == other.mac).count(), 3);
        assert_eq!(clock.windows(8).filter(|w| *w == other.eui64()).count(), 3);
    }

    #[test]
    fn patching_with_the_reference_identity_changes_nothing() {
        // The offsets point at the identity fields and nowhere else.
        let (identity, persona) = rio();
        assert_eq!(body(TYPE_CLOCK, &identity, &persona, 24).unwrap(), hex(CLOCK));
        assert_eq!(body(TYPE_INTERFACE, &identity, &persona, 24).unwrap(), hex(INTERFACE));
        assert_eq!(body(TYPE_SAMPLE_RATE, &identity, &persona, 24).unwrap(), hex(SAMPLE_RATE));
        assert_eq!(body(TYPE_ENCODING, &identity, &persona, 24).unwrap(), hex(ENCODING));
    }

    #[test]
    fn every_query_the_console_sent_has_an_answer() {
        // The console's nine, and the one more Dante Controller asks that a
        // Rio is known to have an answer for.
        for query in [0x0013, 0x0021, 0x0061, 0x0081, 0x0083, 0x0085, 0x00c1, 0x1006, 0x1008, 0x0015] {
            let status = status_for_query(query).expect("a status type");
            let (identity, persona) = rio();
            assert!(body(status, &identity, &persona, 24).is_some());
        }
        assert_eq!(status_for_query(0x9999), None);
    }

    #[test]
    fn an_encoding_write_is_told_from_a_query_and_shows_in_the_status() {
        // Frames 714 and 1034: queries. The trailing bytes are whatever the
        // console had in memory.
        assert_eq!(encoding_write(0x0083, &hex("000f4240000000002937b558")), None);
        assert_eq!(encoding_write(0x0083, &hex("000f42400000000020203020")), None);
        // The write at 68.56 s: 32 bit.
        assert_eq!(encoding_write(0x0083, &hex("000000000000000100000020")), Some(32));
        assert_eq!(encoding_write(0x0081, &hex("000000000000000100000020")), None, "not the encoding type");
        assert_eq!(encoding_write(0x0083, &hex("000000000000000100000099")), None, "not a width");

        // What the Rio published afterwards.
        let (identity, persona) = rio();
        assert_eq!(
            body(TYPE_ENCODING, &identity, &persona, 32).unwrap(),
            hex("00000000001800020000002000000000000200000000001800000020")
        );
    }

    #[test]
    fn a_long_name_is_cut_to_its_field_rather_than_shifting_the_rest() {
        let mut persona = Persona::yamaha_rio3224_d2();
        persona.manufacturer_id = "TooLongMaker".into();
        let b = make_model(&persona);
        assert_eq!(b.len(), 340);
        assert_eq!(&b[4..12], b"TooLongM");
        assert_eq!(&b[12..16], b"8R19");
    }
}

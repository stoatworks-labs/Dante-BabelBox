//! Wire codec for the Sennheiser evolution-wireless G3 **binary WSM
//! protocol** on UDP port 8133. Pure byte-building and parsing — no socket,
//! no async. The [`super::adapter`] module drives it.
//!
//! Every constant, offset and packet shape here was reverse-engineered from
//! Wireless Systems Manager 4.9.0 talking to a **real EM 500 G3** and
//! verified against the hardware on 2026-09-15 (discovery, config read, and
//! writes of frequency, EQ, RX-mute and AF-out all round-tripped). The full
//! write-up and captures live in `~/reverse-engineering/audio/sennheiser-ewg3-re`.
//!
//! This is deliberately NOT the documented ASCII "Media Control Protocol"
//! (TI 1254, UDP 53212): a real EM 500 G3 does not answer 53212 at all (that
//! path needs receiver firmware >= 1.7.0), whereas this 8133 protocol is what
//! WSM actually uses and works on the same unit.

use std::net::Ipv4Addr;

/// UDP port for both directions of the WSM binary protocol.
pub const PORT: u16 = 8133;

/// Discovery multicast group. Note: this is the mDNS group address, but the
/// payload is a fixed Sennheiser record, *not* an mDNS query.
pub const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

// --- 4-byte message-type headers (constant per type; checksum-like, not
// sequential opcodes). Client->device unless noted. -------------------------
const HDR_REGISTER: [u8; 4] = [0x4f, 0x1f, 0xf1, 0xca];
const HDR_CONFIG_REQ: [u8; 4] = [0xa4, 0xfd, 0xf7, 0xca];
const HDR_WRITE: [u8; 4] = [0xb1, 0xf8, 0xf7, 0xca];
/// device->client: full main-config packet.
pub const HDR_CONFIG: [u8; 4] = [0xc8, 0xfc, 0xf7, 0xca];
/// device->client: 40-byte meter/cyclic telemetry (~12 Hz).
pub const HDR_METER: [u8; 4] = [0x29, 0xf8, 0xf7, 0xca];
/// device->client: keepalive ack carrying the client/master table.
pub const HDR_KEEPALIVE_ACK: [u8; 4] = [0x88, 0x23, 0xf1, 0xca];
/// device->client: discovery reply (`Model=..ID=..IPA=..`).
pub const HDR_DISCOVERY_REPLY: [u8; 4] = [0x00, 0x25, 0x12, 0x06];

/// device->client packets carry a 12-byte header: 4-byte type + `0x01` +
/// 6-byte MAC + `0x01`. The body starts after it.
pub const DEVICE_HEADER_LEN: usize = 12;

/// The 120-byte write body's field offsets mirror the config-read body: a
/// value sits at the same offset in both. A write also sets a per-field
/// dirty flag at `FLAG_BASE + offset` and the global flag at `WRITE_FLAG`.
const WRITE_BODY_LEN: usize = 120;
const WRITE_GLOBAL_FLAG: usize = 0x4e;
const FLAG_BASE: usize = 0x45;

// Config/write body field offsets (relative to body start).
const OFF_NAME: usize = 0; // 8 bytes, ASCII space-padded
const OFF_FREQ: usize = 8; // u32 LE, kHz
const OFF_AF_OUT: usize = 0x0e; // AF-out INDEX (see af_* below)
const OFF_EQUALIZER: usize = 0x0f; // 0..3
const OFF_RX_MUTE: usize = 0x10; // 0/1

/// AF-out is stored as an INDEX, not a dB value: `dB = 3*index - 24`, index
/// `0..=14` covering `-24..=+18` dB in 3 dB steps. (WSM ground truth: config
/// byte 0x0c displays "+12", 0x06 displays "-6".) Writing an out-of-range
/// index folds to 0x10 on the device, so callers must clamp.
pub const AF_INDEX_MAX: u8 = 14;
pub const AF_MIN_DB: f32 = -24.0;
pub const AF_MAX_DB: f32 = 18.0;

/// Convert a stored AF-out index to dB.
pub fn af_index_to_db(index: u8) -> f32 {
    3.0 * index as f32 - 24.0
}

/// Convert a requested dB level to the nearest valid AF-out index, clamped
/// to the device's `-24..=+18` dB range.
pub fn af_db_to_index(db: f32) -> u8 {
    let idx = ((db + 24.0) / 3.0).round();
    idx.clamp(0.0, AF_INDEX_MAX as f32) as u8
}

/// Errors parsing a device packet.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("packet too short: {got} bytes, need at least {need}")]
    Truncated { got: usize, need: usize },
    #[error("not a config packet (header {0:02x?})")]
    NotConfig([u8; 4]),
    #[error("discovery reply was not valid UTF-8/ASCII")]
    BadDiscoveryText,
    #[error("discovery reply missing field: {0}")]
    MissingField(&'static str),
}

/// Build the 1035-byte discovery probe (send to [`DISCOVERY_GROUP`]:[`PORT`]).
pub fn discovery_probe() -> Vec<u8> {
    const PREFIX: [u8; 8] = [0x12, 0x07, 0x06, 0x20, 0x00, 0x00, 0x19, 0x00];
    const ASCII: &[u8] = b"[servicecommand]devinfo\r\n";
    let mut p = vec![0u8; 1035];
    p[..PREFIX.len()].copy_from_slice(&PREFIX);
    p[PREFIX.len()..PREFIX.len() + ASCII.len()].copy_from_slice(ASCII);
    let n = p.len();
    p[n - 3..].copy_from_slice(&[0x01, 0x01, 0x01]);
    p
}

/// Build a register / keepalive packet. Resend at < 5 s intervals to keep
/// your slot in the device's client table (and, when no other client holds
/// it, the master slot that gates writes). `token` is this client's 4-byte
/// session id (WSM uses `7f ac aa ec`; any distinct value works).
pub fn register(token: [u8; 4], keepalive: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(18);
    p.extend_from_slice(&HDR_REGISTER);
    p.extend_from_slice(&token);
    p.extend_from_slice(&token);
    p.extend_from_slice(if keepalive { &[0x01, 0x00] } else { &[0x00, 0x00] });
    p.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]);
    p
}

/// Build a full-config request. The device replies with several packets;
/// the main one is [`HDR_CONFIG`], parsed by [`Config::parse`].
pub fn config_request(token: [u8; 4]) -> Vec<u8> {
    let mut p = Vec::with_capacity(11);
    p.extend_from_slice(&HDR_CONFIG_REQ);
    p.extend_from_slice(&token);
    p.extend_from_slice(&[0x01, 0x01, 0x01]);
    p
}

/// Build a single-field write (128 bytes). Sets `body[offset] = value`, the
/// per-field dirty flag, and the global write flag. Honoured only from the
/// master client.
///
/// **Do not use for the name field (offset 0..8).** A stray flag in the
/// `0x45..` region there blanks the device name; the name's real multi-field
/// write path is not reversed. Set the name from WSM.
fn write_field(token: [u8; 4], offset: usize, value: u8) -> Vec<u8> {
    let mut body = [0u8; WRITE_BODY_LEN];
    body[WRITE_GLOBAL_FLAG] = 1;
    body[offset] = value;
    body[FLAG_BASE + offset] = 1;
    let mut p = Vec::with_capacity(8 + WRITE_BODY_LEN);
    p.extend_from_slice(&HDR_WRITE);
    p.extend_from_slice(&token);
    p.extend_from_slice(&body);
    p
}

/// Build a write that sets the AF-out (analog output) level, in dB, clamped
/// to `-24..=+18` and snapped to the nearest 3 dB step.
pub fn set_af_out_db(token: [u8; 4], db: f32) -> Vec<u8> {
    write_field(token, OFF_AF_OUT, af_db_to_index(db))
}

/// Build a write that sets the receiver frequency, in kHz.
pub fn set_frequency_khz(token: [u8; 4], khz: u32) -> Vec<u8> {
    // Frequency is a 4-byte LE field; write each byte with its own dirty flag.
    let mut body = [0u8; WRITE_BODY_LEN];
    body[WRITE_GLOBAL_FLAG] = 1;
    body[OFF_FREQ..OFF_FREQ + 4].copy_from_slice(&khz.to_le_bytes());
    for i in 0..4 {
        body[FLAG_BASE + OFF_FREQ + i] = 1;
    }
    let mut p = Vec::with_capacity(8 + WRITE_BODY_LEN);
    p.extend_from_slice(&HDR_WRITE);
    p.extend_from_slice(&token);
    p.extend_from_slice(&body);
    p
}

/// Build a write that sets RX mute (mutes the receiver's audio output).
pub fn set_rx_mute(token: [u8; 4], on: bool) -> Vec<u8> {
    write_field(token, OFF_RX_MUTE, on as u8)
}

/// A receiver's discovery identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub model: String,
    /// 12 hex chars, e.g. `001B667ADE44`.
    pub id: String,
    pub ip: String,
}

/// Parse a discovery reply payload ([`HDR_DISCOVERY_REPLY`] then
/// `Model=.. ID=.. IPA=..` ASCII, NUL-padded).
pub fn parse_discovery_reply(payload: &[u8]) -> Result<DeviceInfo, Error> {
    if payload.len() < 4 {
        return Err(Error::Truncated { got: payload.len(), need: 4 });
    }
    let text = &payload[4..];
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    let text = std::str::from_utf8(&text[..end]).map_err(|_| Error::BadDiscoveryText)?;
    let mut model = None;
    let mut id = None;
    let mut ip = None;
    for tok in text.split_whitespace() {
        if let Some(v) = tok.strip_prefix("Model=") {
            model = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("ID=") {
            id = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("IPA=") {
            ip = Some(v.to_string());
        }
    }
    Ok(DeviceInfo {
        model: model.ok_or(Error::MissingField("Model"))?,
        id: id.ok_or(Error::MissingField("ID"))?,
        ip: ip.ok_or(Error::MissingField("IPA"))?,
    })
}

/// The decoded, human-relevant fields of a main-config packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub name: String,
    pub frequency_khz: u32,
    pub af_out_db: f32,
    /// 0=flat, 1=low cut, 2=low cut + high boost, 3=high boost.
    pub equalizer: u8,
    pub rx_mute: bool,
}

impl Config {
    /// Parse a full device->client config packet (with its 12-byte header).
    pub fn parse(packet: &[u8]) -> Result<Self, Error> {
        if packet.len() < 4 {
            return Err(Error::Truncated { got: packet.len(), need: 4 });
        }
        let hdr: [u8; 4] = packet[..4].try_into().unwrap();
        if hdr != HDR_CONFIG {
            return Err(Error::NotConfig(hdr));
        }
        Self::parse_body(&packet[DEVICE_HEADER_LEN..]).ok_or(Error::Truncated {
            got: packet.len(),
            need: DEVICE_HEADER_LEN + OFF_RX_MUTE + 1,
        })
    }

    /// Parse just the body (bytes after the 12-byte header).
    pub fn parse_body(body: &[u8]) -> Option<Self> {
        if body.len() <= OFF_RX_MUTE {
            return None;
        }
        let name = std::str::from_utf8(&body[OFF_NAME..OFF_NAME + 8])
            .unwrap_or("")
            .trim_end()
            .to_string();
        let frequency_khz = u32::from_le_bytes(body[OFF_FREQ..OFF_FREQ + 4].try_into().unwrap());
        Some(Config {
            name,
            frequency_khz,
            af_out_db: af_index_to_db(body[OFF_AF_OUT]),
            equalizer: body[OFF_EQUALIZER],
            rx_mute: body[OFF_RX_MUTE] != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exact bytes captured from WSM 4.9.0 <-> EM500 G3 on 2026-09-15.
    const TOKEN: [u8; 4] = [0x7f, 0xac, 0xaa, 0xec];

    #[test]
    fn register_matches_capture() {
        // WSM keepalive: 4f1ff1ca 7facaaec 7facaaec 0100 01010101
        assert_eq!(
            register(TOKEN, true),
            hex("4f1ff1ca7facaaec7facaaec010001010101")
        );
        // non-keepalive variant seen at startup: ...0000...
        assert_eq!(
            register(TOKEN, false),
            hex("4f1ff1ca7facaaec7facaaec000001010101")
        );
    }

    #[test]
    fn config_request_matches_capture() {
        assert_eq!(config_request(TOKEN), hex("a4fdf7ca7facaaec010101"));
    }

    #[test]
    fn discovery_probe_shape() {
        let p = discovery_probe();
        assert_eq!(p.len(), 1035);
        assert_eq!(&p[..8], &[0x12, 0x07, 0x06, 0x20, 0x00, 0x00, 0x19, 0x00]);
        assert_eq!(&p[8..8 + 25], b"[servicecommand]devinfo\r\n");
        assert_eq!(&p[1032..], &[0x01, 0x01, 0x01]);
    }

    #[test]
    fn af_index_math_round_trips_every_step() {
        // Ground truth: index 12 -> +12 dB, index 6 -> -6 dB (from WSM).
        assert_eq!(af_index_to_db(12), 12.0);
        assert_eq!(af_index_to_db(6), -6.0);
        assert_eq!(af_index_to_db(0), -24.0);
        assert_eq!(af_index_to_db(14), 18.0);
        for idx in 0..=AF_INDEX_MAX {
            assert_eq!(af_db_to_index(af_index_to_db(idx)), idx);
        }
    }

    #[test]
    fn af_db_to_index_clamps_and_snaps() {
        assert_eq!(af_db_to_index(-100.0), 0); // below range -> min
        assert_eq!(af_db_to_index(100.0), 14); // above range -> max
        assert_eq!(af_db_to_index(0.0), 8); // 0 dB -> index 8
        assert_eq!(af_db_to_index(1.0), 8); // snaps to nearest 3 dB step
        assert_eq!(af_db_to_index(-6.0), 6);
    }

    #[test]
    fn set_af_out_matches_wsm_write() {
        // WSM's "-6 dB" write body had only body[0x0e]=6, [0x4e]=1, [0x53]=1.
        let pkt = set_af_out_db(TOKEN, -6.0);
        assert_eq!(pkt.len(), 128);
        assert_eq!(&pkt[..4], &HDR_WRITE);
        assert_eq!(&pkt[4..8], &TOKEN);
        let body = &pkt[8..];
        assert_eq!(body[0x0e], 6); // index for -6 dB
        assert_eq!(body[0x4e], 1); // global write flag
        assert_eq!(body[0x45 + 0x0e], 1); // AF-out dirty flag (0x53)
        // nothing else set
        let ones: Vec<usize> = body.iter().enumerate().filter(|(_, &b)| b != 0).map(|(i, _)| i).collect();
        assert_eq!(ones, vec![0x0e, 0x4e, 0x53]);
    }

    #[test]
    fn set_frequency_matches_capture_shape() {
        // 645450 kHz = 0x0009d94a -> LE 4a d9 09 00 at offset 8.
        let pkt = set_frequency_khz(TOKEN, 645450);
        let body = &pkt[8..];
        assert_eq!(&body[8..12], &[0x4a, 0xd9, 0x09, 0x00]);
        for i in 0..4 {
            assert_eq!(body[0x45 + 8 + i], 1);
        }
        assert_eq!(body[0x4e], 1);
    }

    #[test]
    fn parse_config_body_from_capture() {
        // c8fcf7ca body captured at +12 dB, 645.450 MHz, Low Cut, RX-mute off.
        // name "ew500 G3" | freq 4ad90900 (645450 LE) | 14 01 0c(AF=+12) 01(EQ) 00(mute) 08 ...
        let body = hex(
            "65773530302047334ad9090014010c0100080101010101010101000d01000000",
        );
        let c = Config::parse_body(&body).unwrap();
        assert_eq!(c.name, "ew500 G3");
        assert_eq!(c.frequency_khz, 645450);
        assert_eq!(c.af_out_db, 12.0);
        assert_eq!(c.equalizer, 1);
        assert!(!c.rx_mute);
    }

    #[test]
    fn parse_full_config_packet_with_header() {
        // 4-byte type + 01 + MAC + 01 + body.
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&HDR_CONFIG);
        pkt.push(0x01);
        pkt.extend_from_slice(&[0x00, 0x1b, 0x66, 0x7a, 0xde, 0x44]);
        pkt.push(0x01);
        // minimal body: name(8) + freq(4) + up to rx-mute
        let mut body = vec![0u8; 0x11];
        body[..8].copy_from_slice(b"ew500 G3");
        body[8..12].copy_from_slice(&645450u32.to_le_bytes());
        body[0x0e] = 12; // AF index -> +12
        body[0x0f] = 1; // EQ
        body[0x10] = 0; // rx-mute
        pkt.extend_from_slice(&body);
        let c = Config::parse(&pkt).unwrap();
        assert_eq!(c.name, "ew500 G3");
        assert_eq!(c.af_out_db, 12.0);
    }

    #[test]
    fn discovery_reply_parses() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&HDR_DISCOVERY_REPLY);
        payload.extend_from_slice(b"Model=EM500G3   ID=001B667ADE44   IPA=192.168.0.101");
        payload.push(0);
        let info = parse_discovery_reply(&payload).unwrap();
        assert_eq!(info.model, "EM500G3");
        assert_eq!(info.id, "001B667ADE44");
        assert_eq!(info.ip, "192.168.0.101");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}

//! AES70-3 (OCP.1) framing.
//!
//! Wire layout of one PDU:
//!
//! ```text
//! 0        SyncVal   u8   = 0x3B
//! 1        Version   u16  = 1
//! 3        PduSize   u32  octets in this PDU *excluding* SyncVal (so 9 + payload)
//! 7        PduType   u8   see `PduType`
//! 8        MsgCount  u16
//! 10       messages...
//! ```
//!
//! A single PDU carries `MsgCount` messages of one type. We only ever emit one
//! message per PDU; we accept many when reading.

use crate::value::{Reader, Writer};
use crate::Error;

pub const SYNC_VAL: u8 = 0x3B;
pub const PROTOCOL_VERSION: u16 = 1;
/// Bytes from `SyncVal` through `MsgCount` inclusive.
pub const HEADER_LEN: usize = 10;
/// `PduSize` counts everything after `SyncVal`.
const PDU_SIZE_OVERHEAD: usize = HEADER_LEN - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PduType {
    Command = 0,
    CommandResponseRequired = 1,
    Notification = 2,
    Response = 3,
    KeepAlive = 4,
}

impl PduType {
    fn from_u8(v: u8) -> Result<Self, Error> {
        Ok(match v {
            0 => Self::Command,
            1 => Self::CommandResponseRequired,
            2 => Self::Notification,
            3 => Self::Response,
            4 => Self::KeepAlive,
            other => return Err(Error::UnknownPduType(other)),
        })
    }
}

/// Identifies a method or property by its position in the class hierarchy.
///
/// `def_level` is the inheritance depth at which the member is *defined*
/// (`OcaRoot` = 1), and `index` is its 1-based position within that level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberId {
    pub def_level: u16,
    pub index: u16,
}

impl MemberId {
    pub const fn new(def_level: u16, index: u16) -> Self {
        Self { def_level, index }
    }
}

/// A command to be sent to a device.
#[derive(Debug, Clone)]
pub struct Command {
    pub handle: u32,
    pub target: u32,
    pub method: MemberId,
    pub param_count: u8,
    pub params: Vec<u8>,
}

/// A device's reply to a `Command`, correlated by `handle`.
#[derive(Debug, Clone)]
pub struct Response {
    pub handle: u32,
    pub status: u8,
    pub param_count: u8,
    pub params: Vec<u8>,
}

impl Response {
    /// AES70 status code 0 is `OK`; everything else is a failure.
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    pub fn into_result(self) -> Result<Self, Error> {
        if self.ok() {
            Ok(self)
        } else {
            Err(Error::Status { code: self.status, name: status_name(self.status) })
        }
    }
}

/// An unsolicited property-changed event from a device we subscribed to.
#[derive(Debug, Clone)]
pub struct Notification {
    /// The object that actually changed (from the event's emitter field), which
    /// is what a caller wants to match against — not the notification's target,
    /// which is the subscription sink.
    pub emitter: u32,
    pub property: MemberId,
    /// Everything after the property id: the new value, then the change type.
    pub value: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Response(Response),
    Notification(Notification),
    KeepAlive {
        heartbeat_secs: u16,
    },
    /// A command arriving from the far end. We are a controller, not a device,
    /// so we log and ignore these rather than pretend to serve them.
    Command(Command),
}

pub fn status_name(code: u8) -> &'static str {
    match code {
        0 => "OK",
        1 => "ProtocolVersionError",
        2 => "DeviceError",
        3 => "Locked",
        4 => "BadFormat",
        5 => "BadONo",
        6 => "ParameterError",
        7 => "ParameterOutOfRange",
        8 => "NotImplemented",
        9 => "InvalidRequest",
        10 => "ProcessingFailed",
        11 => "BadMethod",
        12 => "PartiallySucceeded",
        13 => "Timeout",
        14 => "BufferOverflow",
        _ => "Unknown",
    }
}

fn write_header(out: &mut Vec<u8>, pdu_type: PduType, payload_len: usize, msg_count: u16) {
    let mut w = Writer::new();
    w.u8(SYNC_VAL)
        .u16(PROTOCOL_VERSION)
        .u32((PDU_SIZE_OVERHEAD + payload_len) as u32)
        .u8(pdu_type as u8)
        .u16(msg_count);
    out.extend_from_slice(&w.finish());
}

/// Encode one command as a complete PDU.
pub fn encode_command(cmd: &Command, response_required: bool) -> Vec<u8> {
    // CommandSize(4) + Handle(4) + TargetONo(4) + MethodID(4) + ParamCount(1)
    const FIXED: usize = 17;
    let body_len = FIXED + cmd.params.len();

    let mut body = Writer::new();
    body.u32(body_len as u32)
        .u32(cmd.handle)
        .u32(cmd.target)
        .u16(cmd.method.def_level)
        .u16(cmd.method.index)
        .u8(cmd.param_count)
        .raw(&cmd.params);
    let body = body.finish();

    let pdu_type =
        if response_required { PduType::CommandResponseRequired } else { PduType::Command };
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    write_header(&mut out, pdu_type, body.len(), 1);
    out.extend_from_slice(&body);
    out
}

pub fn encode_keepalive(heartbeat_secs: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 2);
    write_header(&mut out, PduType::KeepAlive, 2, 1);
    out.extend_from_slice(&heartbeat_secs.to_be_bytes());
    out
}

/// Encode a response PDU. Only a test/mock device needs this — a controller
/// never emits one — but it lives here so the mock in
/// `dante-babelbox-plugin-aes70`'s tests exercises the same encoder the
/// decoder is written against, rather than a hand-rolled second copy.
pub fn encode_response(response: &Response) -> Vec<u8> {
    // ResponseSize(4) + Handle(4) + Status(1) + ParamCount(1)
    const FIXED: usize = 10;
    let body_len = FIXED + response.params.len();

    let mut body = Writer::new();
    body.u32(body_len as u32)
        .u32(response.handle)
        .u8(response.status)
        .u8(response.param_count)
        .raw(&response.params);
    let body = body.finish();

    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    write_header(&mut out, PduType::Response, body.len(), 1);
    out.extend_from_slice(&body);
    out
}

/// How many bytes the PDU starting at `buf` occupies, if its header is complete.
///
/// Returns `Ok(None)` when more bytes are needed.
pub fn pdu_len(buf: &[u8]) -> Result<Option<usize>, Error> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    if buf[0] != SYNC_VAL {
        return Err(Error::BadSync(buf[0]));
    }
    let version = u16::from_be_bytes([buf[1], buf[2]]);
    if version != PROTOCOL_VERSION {
        return Err(Error::BadVersion(version));
    }
    let pdu_size = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]) as usize;
    if pdu_size < PDU_SIZE_OVERHEAD {
        return Err(Error::BadLength(pdu_size));
    }
    let total = pdu_size + 1;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some(total))
}

/// Decode one complete PDU into its constituent messages.
pub fn decode_pdu(pdu: &[u8]) -> Result<Vec<Message>, Error> {
    let total = pdu_len(pdu)?.ok_or(Error::Truncated)?;
    let pdu_type = PduType::from_u8(pdu[7])?;
    let msg_count = u16::from_be_bytes([pdu[8], pdu[9]]);

    let mut r = Reader::new(&pdu[HEADER_LEN..total]);
    let mut out = Vec::with_capacity(msg_count as usize);
    for _ in 0..msg_count {
        out.push(match pdu_type {
            PduType::KeepAlive => decode_keepalive(&mut r)?,
            PduType::Response => decode_response(&mut r)?,
            PduType::Notification => decode_notification(&mut r)?,
            PduType::Command | PduType::CommandResponseRequired => decode_command(&mut r)?,
        });
    }
    Ok(out)
}

fn decode_keepalive(r: &mut Reader<'_>) -> Result<Message, Error> {
    // A device may express its heartbeat in seconds (u16) or milliseconds (u32).
    // Both forms are single-message PDUs, so the remaining length disambiguates.
    let heartbeat_secs = match r.remaining().len() {
        0..=3 => r.u16()?,
        _ => (r.u32()? / 1000).max(1) as u16,
    };
    Ok(Message::KeepAlive { heartbeat_secs })
}

fn decode_response(r: &mut Reader<'_>) -> Result<Message, Error> {
    let size = r.u32()? as usize;
    let handle = r.u32()?;
    let status = r.u8()?;
    let param_count = r.u8()?;
    // ResponseSize covers itself, so subtract the fixed part to find the params.
    let params_len = size.checked_sub(10).ok_or(Error::BadLength(size))?;
    let params = r.remaining().get(..params_len).ok_or(Error::Truncated)?.to_vec();
    *r = Reader::new(&r.remaining()[params_len..]);
    Ok(Message::Response(Response { handle, status, param_count, params }))
}

fn decode_command(r: &mut Reader<'_>) -> Result<Message, Error> {
    let size = r.u32()? as usize;
    let handle = r.u32()?;
    let target = r.u32()?;
    let method = MemberId::new(r.u16()?, r.u16()?);
    let param_count = r.u8()?;
    let params_len = size.checked_sub(17).ok_or(Error::BadLength(size))?;
    let params = r.remaining().get(..params_len).ok_or(Error::Truncated)?.to_vec();
    *r = Reader::new(&r.remaining()[params_len..]);
    Ok(Message::Command(Command { handle, target, method, param_count, params }))
}

fn decode_notification(r: &mut Reader<'_>) -> Result<Message, Error> {
    let size = r.u32()? as usize;
    let body_len = size.checked_sub(4).ok_or(Error::BadLength(size))?;
    let body = r.remaining().get(..body_len).ok_or(Error::Truncated)?;
    *r = Reader::new(&r.remaining()[body_len..]);

    // NotificationSize | TargetONo | MethodID | ParamCount | params...
    // and for OcaRoot::PropertyChanged the params are:
    //   OcaBlob NotificationID | OcaEventID | OcaPropertyID | value | changeType
    let mut b = Reader::new(body);
    let _target = b.u32()?;
    let _method = MemberId::new(b.u16()?, b.u16()?);
    let _param_count = b.u8()?;
    let _notification_id = b.bytes()?;
    let emitter = b.u32()?;
    let _event = MemberId::new(b.u16()?, b.u16()?);
    let property = MemberId::new(b.u16()?, b.u16()?);

    Ok(Message::Notification(Notification { emitter, property, value: b.remaining().to_vec() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical 12-byte keepalive frame: sync, v1, size 11, type 4, one
    /// message, 5 second heartbeat.
    const KEEPALIVE_5S: &[u8] =
        &[0x3B, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0B, 0x04, 0x00, 0x01, 0x00, 0x05];

    #[test]
    fn encodes_the_canonical_keepalive_frame() {
        assert_eq!(encode_keepalive(5), KEEPALIVE_5S);
    }

    #[test]
    fn pdu_size_excludes_the_sync_byte() {
        let len = pdu_len(KEEPALIVE_5S).unwrap().unwrap();
        assert_eq!(len, 12);
        assert_eq!(u32::from_be_bytes([0, 0, 0, 0x0B]) as usize + 1, len);
    }

    #[test]
    fn decodes_keepalive() {
        match decode_pdu(KEEPALIVE_5S).unwrap().as_slice() {
            [Message::KeepAlive { heartbeat_secs: 5 }] => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn command_layout_matches_the_documented_offsets() {
        let cmd = Command {
            handle: 0x0000_0001,
            target: 0x0000_0004,
            method: MemberId::new(3, 1),
            param_count: 1,
            params: vec![0xAA, 0xBB],
        };
        let pdu = encode_command(&cmd, true);

        assert_eq!(pdu[0], SYNC_VAL);
        assert_eq!(pdu[7], PduType::CommandResponseRequired as u8);
        // CommandSize at 10, Handle at 14, TargetONo at 18, MethodID at 22.
        assert_eq!(&pdu[10..14], &(19u32).to_be_bytes());
        assert_eq!(&pdu[14..18], &1u32.to_be_bytes());
        assert_eq!(&pdu[18..22], &4u32.to_be_bytes());
        assert_eq!(&pdu[22..24], &3u16.to_be_bytes());
        assert_eq!(&pdu[24..26], &1u16.to_be_bytes());
        assert_eq!(pdu[26], 1);
        assert_eq!(&pdu[27..], &[0xAA, 0xBB]);
        assert_eq!(pdu_len(&pdu).unwrap().unwrap(), pdu.len());
    }

    #[test]
    fn round_trips_a_response_through_its_own_encoder() {
        let response = Response { handle: 7, status: 0, param_count: 1, params: vec![0x12, 0x34] };
        let pdu = encode_response(&response);
        assert_eq!(pdu_len(&pdu).unwrap().unwrap(), pdu.len());

        match decode_pdu(&pdu).unwrap().as_slice() {
            [Message::Response(r)] => {
                assert_eq!(r.handle, 7);
                assert!(r.ok());
                assert_eq!(r.param_count, 1);
                assert_eq!(r.params, vec![0x12, 0x34]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn non_zero_status_becomes_an_error() {
        let r = Response { handle: 1, status: 5, param_count: 0, params: vec![] };
        let err = r.into_result().unwrap_err();
        assert!(matches!(err, Error::Status { code: 5, name: "BadONo" }));
    }

    #[test]
    fn pdu_len_waits_for_more_bytes() {
        assert_eq!(pdu_len(&KEEPALIVE_5S[..4]).unwrap(), None);
        assert_eq!(pdu_len(&KEEPALIVE_5S[..11]).unwrap(), None);
        assert!(matches!(pdu_len(&[0x00; 12]), Err(Error::BadSync(0))));
    }

    #[test]
    fn decodes_a_property_changed_notification() {
        let mut params = Writer::new();
        params
            .bytes(&[0xDE, 0xAD]) // NotificationID
            .u32(0x0100_8206) // emitter ONo
            .u16(1)
            .u16(1) // event id (OcaRoot::PropertyChanged)
            .u16(4)
            .u16(1) // property id (OcaGain::Gain)
            .f32(-6.0) // new value
            .u8(1); // change type
        let params = params.finish();

        let mut body = Writer::new();
        // NotificationSize covers itself(4) + target(4) + method(4) + count(1).
        body.u32((13 + params.len()) as u32).u32(0x0000_0004).u16(3).u16(1).u8(3).raw(&params);
        let body = body.finish();
        let mut pdu = Vec::new();
        write_header(&mut pdu, PduType::Notification, body.len(), 1);
        pdu.extend_from_slice(&body);

        match decode_pdu(&pdu).unwrap().as_slice() {
            [Message::Notification(n)] => {
                assert_eq!(n.emitter, 0x0100_8206);
                assert_eq!(n.property, MemberId::new(4, 1));
                assert_eq!(Reader::new(&n.value).f32().unwrap(), -6.0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

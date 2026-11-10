//! The device side of Audinate's ARC protocol (UDP 4440 by default): what a
//! Dante device answers when a controller asks what it is, what channels it
//! has and what they are subscribed to.
//!
//! The workspace's own ARC *client* is
//! `dante_babelbox_discovery::dante_control`; this module is its mirror
//! image, and `tests/arc_client.rs` runs the one against the other.
//!
//! ## Sources
//!
//! - `ql1-rio3224d2-pairing-gain-phantom.control.pcap` (private
//!   `dante-captures` repo): a QL1 put 64 ARC requests to a Rio3224-D2 in
//!   protocol version `0x2801`. Every reply format below was read off the
//!   Rio's answers, and `examples/replay_capture.rs` puts the console's
//!   requests to this module and compares the answers byte for byte.
//! - The request and record layouts shared with the older `0x27ff` version
//!   are documented, with known-answer tests, in
//!   chris-ritsen/network-audio-controller's `netaudio-core` (Unlicense):
//!   <https://github.com/chris-ritsen/network-audio-controller>.
//!
//! - `yamaha-dm3/dm3-dante-cmc-arc.pcapng` (same repo): Dante Controller
//!   asking a DM3's Dante module in protocol version `0x2808`. Dante
//!   Controller asks for channel lists in a second, richer layout (`0x2002`,
//!   `0x2400`, `0x3400`) that the console never used. Those layouts are
//!   built here as that module built them. **No capture exists of Dante
//!   Controller asking a Rio**, so for these replies the layout is one real
//!   module's and the content is the persona's; whether a Rio answers them
//!   the same way, or at all, is not known.
//!
//! ## What is and is not understood
//!
//! Names, counts, pointers and the paging of channel lists are understood
//! and built from the persona. Several replies also carry words whose
//! meaning is unknown; those are sent exactly as the Rio sent them and are
//! marked "as captured" where they appear. A virtual device carries no
//! audio, so it reports no flows, and a subscription a controller sets on
//! one of its receive channels is stored and reported back as unresolved.
//!
//! Every pointer in a reply counts from the start of the packet.

use std::collections::BTreeMap;

use crate::hexutil::hex;
use crate::identity::DeviceIdentity;
use crate::persona::Persona;

pub const DEFAULT_PORT: u16 = 4440;

pub const OP_CHANNEL_COUNT: u16 = 0x1000;
pub const OP_DEVICE_NAME: u16 = 0x1002;
pub const OP_DEVICE_INFO: u16 = 0x1003;
pub const OP_PROPERTY_GET: u16 = 0x1100;
pub const OP_PROPERTY_SET: u16 = 0x1101;
pub const OP_PROPERTY_LIST: u16 = 0x1102;
pub const OP_1F11: u16 = 0x1f11;
pub const OP_TX_CHANNELS: u16 = 0x2000;
pub const OP_TX_LABELS: u16 = 0x2010;
pub const OP_2032: u16 = 0x2032;
pub const OP_TX_CHANNELS_V2: u16 = 0x2002;
pub const OP_TX_FLOWS: u16 = 0x2200;
pub const OP_2204: u16 = 0x2204;
pub const OP_2320: u16 = 0x2320;
pub const OP_TX_CHANNELS_FULL: u16 = 0x2400;
pub const OP_TX_FLOWS_V2: u16 = 0x2600;
pub const OP_RX_CHANNELS: u16 = 0x3000;
pub const OP_SUBSCRIBE: u16 = 0x3010;
pub const OP_UNSUBSCRIBE: u16 = 0x3014;
pub const OP_RX_FLOWS: u16 = 0x3200;
pub const OP_RX_CHANNELS_V2: u16 = 0x3400;
pub const OP_RX_FLOWS_V2: u16 = 0x3600;
pub const OP_3210: u16 = 0x3210;
pub const OP_3300: u16 = 0x3300;

/// Reply status: done.
pub const STATUS_OK: u16 = 0x0001;
/// Reply status: done, and the list continues on the next page. The Rio
/// answered the first page of its 24 receive channels with this and the
/// second with [`STATUS_OK`].
pub const STATUS_MORE: u16 = 0x8112;
/// Reply status the DM3's module gave, with no body, to the one request it
/// would not answer (`0x2320`). Read as "no", and used only there.
pub const STATUS_REFUSED: u16 = 0x0030;

/// A reply's header: version, length, transaction, opcode, status.
pub const REPLY_HEADER_LEN: usize = 10;
/// A request's header is the same without the status word. Its body opens
/// with a zero word where a reply has its status, so the two line up.
const REQUEST_BODY: usize = 8;
const TX_PAGE: u16 = 32;
const RX_PAGE: u16 = 16;
/// Channels to a page in the second-generation listings. Dante Controller
/// asked for 16 at a time, and 32 of the larger records would not fit one
/// datagram.
const V2_PAGE: u16 = 16;
const TX_RECORD: usize = 8;
const RX_RECORD: usize = 20;

/// Subscription state word for a receive channel whose transmitter cannot
/// be found. The Rio reported this for channels subscribed to a console
/// that was not on the network, which is the situation of every
/// subscription on a device that carries no audio.
pub const RX_STATUS_UNRESOLVED: u32 = 0x0000_0001;

/// What one receive channel is subscribed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub tx_channel: String,
    pub tx_device: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Property {
    /// Value carried in the entry itself.
    Inline(u16),
    /// Value carried after the entries, with a pointer in the entry. Ids
    /// with the top bit set work this way.
    Blob(Vec<u8>),
}

/// The part of a device's state a controller can change.
#[derive(Debug, Clone)]
pub struct ArcState {
    /// One slot per receive channel, index 0 is channel 1: the subscription
    /// and its state word.
    subscriptions: Vec<Option<(Subscription, u32)>>,
    properties: BTreeMap<u16, Property>,
    /// Sample width in bits. Changed over ConMon, not ARC (see
    /// [`crate::status::encoding_write`]), but it shows in the format block
    /// of every channel list, so it is kept here.
    encoding_bits: u16,
}

impl ArcState {
    pub fn new(persona: &Persona) -> Self {
        // The Rio's own answer to the console's property read, before the
        // console changed anything. The ids are Audinate's and mostly
        // undecoded; 0x8205 / 0x8301 are latency in nanoseconds (the console
        // wrote 250 000 to both when it set 0.25 ms), and 0x8204 reads as
        // the 1 ms default.
        let blob = |h: &str| Property::Blob(hex(h));
        let properties = BTreeMap::from([
            (0x0201, Property::Inline(0x0001)),
            (0x8204, blob("000f4240")),
            (0x8205, blob("000f4240")),
            (0x0210, Property::Inline(0x0010)),
            (0x0211, Property::Inline(0x0010)),
            (0x8301, blob("000f4240")),
            (0x8302, blob("028b0aab")),
            (0x8306, blob("0003d090")),
            (0x0310, Property::Inline(0x0010)),
            (0x0311, Property::Inline(0x0002)),
            (0x0303, Property::Inline(0x0004)),
            (0x8021, Property::Blob(vec![0; 16])),
            (0x00f0, Property::Inline(0x0000)),
            (0x8060, blob("ef450000")),
            (0x0022, Property::Inline(0x0001)),
            (0x0063, Property::Inline(0x0001)),
            // The console never asked the Rio for the rest of its property
            // list. Dante Controller asks for all of it, so the gaps are
            // filled from the one module that was seen to answer in full, a
            // DM3's. They are that module's values, not a Rio's; where both
            // devices were asked the same property they agreed on all but
            // one. 0x8020 is the sample rate.
            (0x8020, Property::Blob(persona.sample_rate.to_be_bytes().to_vec())),
            (0x0023, Property::Inline(0x0000)),
            (0x0024, Property::Inline(0x0001)),
            (0x0062, Property::Inline(0x0001)),
            (0x020a, Property::Inline(0x0000)),
            (0x020b, Property::Inline(0x0000)),
            (0x0212, Property::Inline(0x0030)),
            (0x0213, Property::Inline(0x0002)),
            (0x0214, Property::Inline(0x0040)),
            (0x0222, Property::Inline(0x138c)),
            (0x8321, blob("001e8480")),
            (0x0312, Property::Inline(0x0030)),
            (0x83f0, blob("0000000500000000")),
            (0x0601, Property::Inline(0x0000)),
            (0x0309, Property::Inline(0x0003)),
            (0x0209, Property::Inline(0x0001)),
        ]);
        Self {
            subscriptions: vec![None; persona.rx_channels as usize],
            properties,
            encoding_bits: crate::status::DEFAULT_ENCODING_BITS,
        }
    }

    /// What receive channel `channel` (1-based) is subscribed to.
    pub fn subscription(&self, channel: u16) -> Option<&Subscription> {
        self.slot(channel).map(|(subscription, _)| subscription)
    }

    fn slot(&self, channel: u16) -> Option<&(Subscription, u32)> {
        self.subscriptions.get(channel.checked_sub(1)? as usize)?.as_ref()
    }

    /// Puts a subscription in place with a given state word, as if the
    /// device had come up already holding it. A real device keeps its
    /// subscriptions across power cycles; the capture replay uses this to
    /// start from the state the captured device was in.
    pub fn restore_subscription(&mut self, channel: u16, subscription: Subscription, state_word: u32) {
        if let Some(slot) = channel.checked_sub(1).and_then(|i| self.subscriptions.get_mut(i as usize)) {
            *slot = Some((subscription, state_word));
        }
    }

    /// Puts raw bytes into an out-of-line property, as if the device had
    /// come up holding them; for the capture replay, like
    /// [`restore_subscription`](Self::restore_subscription).
    pub fn restore_property(&mut self, id: u16, value: &[u8]) {
        self.properties.insert(id, Property::Blob(value.to_vec()));
    }

    /// An out-of-line property's bytes.
    fn blob(&self, id: u16) -> &[u8] {
        match self.properties.get(&id) {
            Some(Property::Blob(bytes)) => bytes,
            _ => &[],
        }
    }

    pub fn encoding_bits(&self) -> u16 {
        self.encoding_bits
    }

    pub fn set_encoding_bits(&mut self, bits: u16) {
        self.encoding_bits = bits;
    }

    fn blob_len(&self, id: u16) -> usize {
        match self.properties.get(&id) {
            Some(Property::Blob(b)) => b.len(),
            _ => 4,
        }
    }
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

fn c_string(data: &[u8], at: usize) -> Option<String> {
    let tail = data.get(at..)?;
    let end = tail.iter().position(|&b| b == 0)?;
    String::from_utf8(tail[..end].to_vec()).ok()
}

/// A reply under construction: the header, then a body whose pointers are
/// offsets from the start of the packet.
struct Reply {
    bytes: Vec<u8>,
}

impl Reply {
    fn new(request: &[u8], status: u16) -> Self {
        Self::in_version([request[0], request[1]], request, status) // version echoed
    }

    /// A reply that names its own protocol version instead of echoing the
    /// request's.
    fn in_version(version: [u8; 2], request: &[u8], status: u16) -> Self {
        let mut bytes = Vec::with_capacity(1024);
        bytes.extend_from_slice(&version);
        bytes.extend_from_slice(&[0, 0]); // length, set in finish()
        bytes.extend_from_slice(&request[4..6]); // transaction id
        bytes.extend_from_slice(&request[6..8]); // opcode
        bytes.extend_from_slice(&status.to_be_bytes());
        Self { bytes }
    }

    fn put(&mut self, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(data);
        self
    }

    fn put_u16(&mut self, value: u16) -> &mut Self {
        self.put(&value.to_be_bytes())
    }

    fn zeros(&mut self, count: usize) -> &mut Self {
        self.bytes.resize(self.bytes.len() + count, 0);
        self
    }

    fn here(&self) -> u16 {
        self.bytes.len() as u16
    }

    fn set_u16(&mut self, at: usize, value: u16) {
        self.bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// Appends a NUL-terminated string and returns where it starts.
    fn string(&mut self, text: &str) -> u16 {
        let at = self.here();
        self.put(text.as_bytes());
        self.bytes.push(0);
        at
    }

    fn align4(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        let total = self.bytes.len() as u16;
        self.set_u16(2, total);
        self.bytes
    }
}

/// The 16-byte audio format block channel records point at: sample rate,
/// `0101 0018 0400` as captured, the sample width twice, `000c` as captured.
/// The two width words followed the console's encoding write (24 to 32);
/// the `0018` before them did not.
fn format_block(persona: &Persona, encoding_bits: u16) -> Vec<u8> {
    let mut block = persona.sample_rate.to_be_bytes().to_vec();
    block.extend_from_slice(&hex("0101 0018 0400"));
    block.extend_from_slice(&encoding_bits.to_be_bytes());
    block.extend_from_slice(&encoding_bits.to_be_bytes());
    block.extend_from_slice(&hex("000c"));
    block
}

/// Answers one ARC request. `None` means "say nothing": the request is not
/// ARC, or it is an opcode no reply was ever captured for.
pub fn respond(
    request: &[u8],
    identity: &DeviceIdentity,
    persona: &Persona,
    state: &mut ArcState,
) -> Option<Vec<u8>> {
    if request.len() < 8 {
        return None;
    }
    // Every ARC version seen starts 0x27 or 0x28 (0x27ff, 0x2801, 0x2808).
    if !matches!(request[0], 0x27 | 0x28) {
        return None;
    }
    let opcode = u16_at(request, 6)?;
    Some(match opcode {
        OP_CHANNEL_COUNT => channel_count(request, persona),
        OP_DEVICE_NAME => {
            let mut reply = Reply::new(request, STATUS_OK);
            reply.string(&identity.name);
            reply.finish()
        }
        OP_DEVICE_INFO => device_info(request, identity, persona),
        OP_PROPERTY_LIST => {
            let mut reply = Reply::new(request, STATUS_OK);
            reply.put(&hex(PROPERTY_LIST));
            reply.finish()
        }
        OP_PROPERTY_GET => property_get(request, state)?,
        OP_PROPERTY_SET => property_set(request, state)?,
        OP_TX_CHANNELS => tx_channels(request, persona, state)?,
        OP_TX_LABELS => {
            // No channel has been given a label: as many empty 8-byte slots
            // as were asked for (32 by the console, 16 by Dante Controller).
            let asked = u16_at(request, REQUEST_BODY + 6).unwrap_or(0);
            let slots = if asked == 0 { TX_PAGE } else { asked.min(TX_PAGE) };
            let mut reply = Reply::new(request, STATUS_OK);
            reply.put(&[slots as u8, 0]).zeros(slots as usize * 8);
            reply.finish()
        }
        OP_TX_CHANNELS_V2 => tx_channels_v2(request, persona, state)?,
        OP_TX_CHANNELS_FULL => channels_full(request, persona, state, Direction::Tx)?,
        OP_RX_CHANNELS_V2 => channels_full(request, persona, state, Direction::Rx)?,
        OP_TX_FLOWS_V2 | OP_RX_FLOWS_V2 => {
            // The second-generation flow lists: sixteen slots, none in use,
            // which is what the DM3's module answered for both.
            let mut reply = Reply::new(request, STATUS_OK);
            reply.zeros(6).put(&[0x10, 0]).zeros(32);
            reply.finish()
        }
        OP_2204 => {
            // As the DM3's module answered it, version word included: it
            // sent this one reply as 0x2801 to a 0x2808 request.
            let mut reply = Reply::in_version([0x28, 0x01], request, STATUS_OK);
            reply.put(&hex("0400 0000 0000 0000 0000"));
            reply.finish()
        }
        // The one request the DM3's module refused. What it asks is unknown.
        OP_2320 => Reply::new(request, STATUS_REFUSED).finish(),
        OP_2032 => {
            // As captured, with the transmit channel count in the third word.
            let mut reply = Reply::new(request, STATUS_OK);
            reply.put_u16(1).put_u16(1).put_u16(persona.tx_channels).put_u16(0x7fff);
            reply.finish()
        }
        OP_TX_FLOWS | OP_RX_FLOWS => {
            // Sixteen flow slots, none in use. This is the Rio's own answer
            // for transmit flows before the console subscribed to it. For
            // receive flows the Rio was never seen without one, so the same
            // empty shape is assumed there.
            let mut reply = Reply::new(request, STATUS_OK);
            reply.put(&[0x10, 0]).zeros(32);
            reply.finish()
        }
        OP_RX_CHANNELS => rx_channels(request, persona, state)?,
        OP_SUBSCRIBE => {
            subscribe(request, state);
            Reply::new(request, STATUS_OK).finish()
        }
        OP_UNSUBSCRIBE => {
            unsubscribe(request, state);
            Reply::new(request, STATUS_OK).finish()
        }
        // The Rio acknowledged these with an empty reply; what they ask for
        // is not known.
        OP_1F11 | OP_3210 => Reply::new(request, STATUS_OK).finish(),
        OP_3300 => {
            // As captured: two port ranges, 14336-14719 and 14720-14847.
            let mut reply = Reply::new(request, STATUS_OK);
            reply.put(&hex("3800397f398039ff"));
            reply.finish()
        }
        _ => return None,
    })
}

/// `0x1000`: transmit and receive channel counts, then nine words as
/// captured (they read like flow and per-flow limits).
fn channel_count(request: &[u8], persona: &Persona) -> Vec<u8> {
    let mut reply = Reply::new(request, STATUS_OK);
    reply
        .put_u16(0x0ff9)
        .put_u16(persona.tx_channels)
        .put_u16(persona.rx_channels)
        .put(&hex("0000 0040 0040 0020 0020 0020 0001 0001"))
        .zeros(16);
    reply.finish()
}

/// `0x1003`: the device's name, its module's host name and the router's
/// build strings.
fn device_info(request: &[u8], identity: &DeviceIdentity, persona: &Persona) -> Vec<u8> {
    fn field(reply: &mut Reply, text: &str, width: usize) -> u16 {
        let at = reply.here();
        let bytes = text.as_bytes();
        let take = bytes.len().min(width - 1);
        reply.put(&bytes[..take]).zeros(width - take);
        at
    }

    let mut reply = Reply::new(request, STATUS_OK);
    // Five pointers, filled in below, then padding to the first block.
    let pointers = reply.here() as usize;
    reply.zeros(18);

    let names_block = reply.here();
    reply.put_u16(0x0500);
    let name_pointers = reply.here() as usize;
    reply.zeros(10);

    let versions_block = reply.here();
    // As captured. Readable in it: router 4.3, ARC 2.8.9 with minimum
    // 0.2.4, and the 0x1200 / 0x1004 the channel adverts call dbcp1 / dbcp.
    reply.put(&hex("0a0a 0000 0403 0000 2809 0204 1200 1004 0000 0000"));

    let name = field(&mut reply, &identity.name, 32);
    let host = field(&mut reply, &module_host_name(identity, persona), 32);
    let router = reply.string("Audinate DCM");
    let build = reply.string(":800");

    reply.set_u16(pointers, names_block);
    reply.set_u16(pointers + 2, names_block);
    reply.set_u16(pointers + 4, versions_block);
    reply.set_u16(pointers + 6, router);
    reply.set_u16(pointers + 8, build);
    reply.set_u16(name_pointers, name);
    reply.set_u16(name_pointers + 2, host);
    reply.set_u16(name_pointers + 4, name);
    reply.finish()
}

/// `BROOKLYN3-25df04`: the Dante module's own host name.
pub fn module_host_name(identity: &DeviceIdentity, persona: &Persona) -> String {
    format!("{}-{}", persona.module_host_prefix, identity.mac_suffix())
}

/// `0x1102`: the property ids the device has, each with a type word. As
/// captured from the Rio.
const PROPERTY_LIST: &str = "0020802000018021000300220003002300030024000180600003006200030063\
    000100f00003020100038204000382050003020a0001020b0001021000030211\
    0003021200030213000102140001022200038301000383060001830200018321\
    00010310000303110001031200010303000383f0000106010001030900010209\
    0001";

/// `0x1100`: read the listed properties.
///
/// Request body: `0000 | count | ids`. Reply body: `count count`, an
/// `id value` entry per property, then the out-of-line values in order.
///
/// A request with no list at all (Dante Controller's first read) asks for
/// everything, in the order of the property list.
fn property_get(request: &[u8], state: &ArcState) -> Option<Vec<u8>> {
    let count = u16_at(request, REQUEST_BODY + 2).unwrap_or(0) as usize;
    if count == 0 {
        return Some(property_reply(request, &listed_properties(), state));
    }
    let ids: Vec<u16> =
        (0..count).map(|i| u16_at(request, REQUEST_BODY + 4 + i * 2)).collect::<Option<_>>()?;
    Some(property_reply(request, &ids, state))
}

/// The ids in [`PROPERTY_LIST`], in its order: a count, then `id type` pairs.
fn listed_properties() -> Vec<u16> {
    let list = hex(PROPERTY_LIST);
    list[2..].as_chunks::<4>().0.iter().map(|entry| u16::from_be_bytes([entry[0], entry[1]])).collect()
}

fn property_reply(request: &[u8], ids: &[u16], state: &ArcState) -> Vec<u8> {
    let mut reply = Reply::new(request, STATUS_OK);
    reply.put(&[ids.len() as u8, ids.len() as u8]);
    let entries = reply.here() as usize;
    reply.zeros(ids.len() * 4);
    for (i, &id) in ids.iter().enumerate() {
        let value = match state.properties.get(&id) {
            Some(Property::Inline(v)) => *v,
            Some(Property::Blob(blob)) => {
                let at = reply.here();
                reply.put(blob);
                at
            }
            // A property the device does not have. The DM3's module was
            // asked for five such, and answered each with a zero where the
            // id goes and the id where the value goes.
            None => {
                reply.set_u16(entries + i * 4 + 2, id);
                continue;
            }
        };
        reply.set_u16(entries + i * 4, id);
        reply.set_u16(entries + i * 4 + 2, value);
    }
    reply.finish()
}

/// `0x1101`: write properties. The reply reads the same ones back.
///
/// Request body: `0000`, one byte as captured (`05`), a count, an `id
/// value` entry per property (a pointer for out-of-line ids), then the
/// values.
fn property_set(request: &[u8], state: &mut ArcState) -> Option<Vec<u8>> {
    let count = *request.get(REQUEST_BODY + 3)? as usize;
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let entry = REQUEST_BODY + 4 + i * 4;
        let id = u16_at(request, entry)?;
        let value = u16_at(request, entry + 2)?;
        let property = if id & 0x8000 != 0 {
            let at = value as usize;
            Property::Blob(request.get(at..at + state.blob_len(id))?.to_vec())
        } else {
            Property::Inline(value)
        };
        state.properties.insert(id, property);
        ids.push(id);
    }
    Some(property_reply(request, &ids, state))
}

/// The first channel a paged request asks for (1-based). Request body:
/// `0000 | 0001 | first channel | ...`.
fn start_channel(request: &[u8]) -> Option<u16> {
    u16_at(request, REQUEST_BODY + 4)
}

/// `0x2000`: transmit channels, 32 to a page.
///
/// Body: `32 | n`, then per channel `number | 0107 | format pointer | name
/// pointer`, the format block, and the names.
fn tx_channels(request: &[u8], persona: &Persona, state: &ArcState) -> Option<Vec<u8>> {
    let start = start_channel(request)?.max(1);
    let last = persona.tx_channels.min(start.saturating_add(TX_PAGE - 1));
    let channels: Vec<u16> = (start..=last).collect();
    let status = if last < persona.tx_channels { STATUS_MORE } else { STATUS_OK };

    let mut reply = Reply::new(request, status);
    reply.put(&[TX_PAGE as u8, channels.len() as u8]);
    let records = reply.here() as usize;
    reply.zeros(channels.len() * TX_RECORD);
    reply.align4();
    let format = reply.here();
    reply.put(&format_block(persona, state.encoding_bits));
    for (i, &channel) in channels.iter().enumerate() {
        let name = reply.string(&persona.tx_channel_name(channel));
        let at = records + i * TX_RECORD;
        reply.set_u16(at, channel);
        reply.set_u16(at + 2, 0x0107); // as captured
        reply.set_u16(at + 4, format);
        reply.set_u16(at + 6, name);
    }
    Some(reply.finish())
}

/// The channels a second-generation listing covers: the page, how many slots
/// the reply announces, and whether more follow.
fn v2_page(start: u16, asked: u16, total: u16) -> (Vec<u16>, u8, u16) {
    let start = start.max(1);
    let slots = if asked == 0 { V2_PAGE } else { asked.min(V2_PAGE) };
    let last = total.min(start.saturating_add(slots - 1));
    let status = if last < total { STATUS_MORE } else { STATUS_OK };
    ((start..=last).collect(), slots as u8, status)
}

/// `0x2002`: transmit channels, second generation, as a DM3's module
/// answered Dante Controller.
///
/// Request body: `0000 | 0001 | first channel | how many`. Reply body: six
/// zero bytes, a pointer to the counts and one to block A, `slots | n`, a
/// pointer per slot, then two shared blocks and, per channel, its name and a
/// 20-byte record `0a00 | number | name | 0000 | block B | format | 0000 |
/// 7fff | number | 0000`. The format block follows the first record.
///
/// Block A (`0500 1200 1167 ...`) is sent as captured. Block B is `0600
/// 0010`, the device's latency in nanoseconds (it matched property
/// `0x8205`), then `0010 0002`, which reads like the `fpp=16,2` of the mDNS
/// channel records.
fn tx_channels_v2(request: &[u8], persona: &Persona, state: &ArcState) -> Option<Vec<u8>> {
    let (channels, slots, status) = v2_page(
        u16_at(request, REQUEST_BODY + 4)?,
        u16_at(request, REQUEST_BODY + 6)?,
        persona.tx_channels,
    );
    let mut reply = Reply::new(request, status);
    reply.zeros(6);
    let block_pointers = reply.here() as usize;
    reply.zeros(4);
    let counts = reply.here();
    reply.put(&[slots, channels.len() as u8]);
    let pointers = reply.here() as usize;
    reply.zeros(slots as usize * 2);
    reply.align4();
    let block_a = reply.here();
    reply.put(&hex("0500 1200 1167 0000 0000 0000"));
    let block_b = reply.here();
    let mut latency = state.blob(0x8205).to_vec();
    latency.resize(4, 0);
    reply.put(&hex("0600 0010")).put(&latency).put(&hex("0010 0002"));
    reply.set_u16(block_pointers, counts);
    reply.set_u16(block_pointers + 2, block_a);

    let mut format: Option<u16> = None;
    for (i, &channel) in channels.iter().enumerate() {
        let name = reply.string(&persona.tx_channel_name(channel));
        reply.align4();
        let record = reply.here();
        // The first record points forward at the format block behind it.
        let format_at = format.unwrap_or(record + 20);
        reply
            .put_u16(0x0a00)
            .put_u16(channel)
            .put_u16(name)
            .put_u16(0)
            .put_u16(block_b)
            .put_u16(format_at)
            .put_u16(0)
            .put_u16(0x7fff)
            .put_u16(channel)
            .put_u16(0);
        if format.is_none() {
            format = Some(reply.here());
            reply.put(&format_block(persona, state.encoding_bits));
        }
        reply.set_u16(pointers + i * 2, record);
    }
    Some(reply.finish())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Tx,
    Rx,
}

/// `0x2400` (transmit) and `0x3400` (receive): the full channel listings
/// Dante Controller reads, as a DM3's module answered them.
///
/// Request body: `0000`, eight zero bytes, `0001 | first channel | 0001 |
/// how many`, more zeros. (The three ones were all `0001` in every request
/// captured, so which of them is the first channel is a reading, not a
/// certainty.) Reply body: six zero bytes, `slots | n`, a pointer per slot,
/// the format block, then per channel its name and a record of twenty words:
///
/// ```text
/// 1414 | number | 0000 | 0003 | number | 0000 | 0000 | 0107 | 0000 | 0000 |
/// name | format | 0000 | 0000 | 0000 | name | 0000 | 0000 | 0000 | 0000
/// ```
///
/// A receive record opens `141c`, has `000e` where a transmit one has
/// `0107` (the same two words the first-generation records carry), and adds
/// eight more words for its subscription: `0608`, five zeros, `0202`, zero.
/// That is the only form captured, a channel subscribed to nothing, so it
/// is the only form sent: **a subscription a controller sets shows in the
/// first-generation listing (`0x3000`) and not in this one.**
fn channels_full(
    request: &[u8],
    persona: &Persona,
    state: &ArcState,
    direction: Direction,
) -> Option<Vec<u8>> {
    let total = match direction {
        Direction::Tx => persona.tx_channels,
        Direction::Rx => persona.rx_channels,
    };
    let (channels, slots, status) = v2_page(
        u16_at(request, REQUEST_BODY + 12)?,
        u16_at(request, REQUEST_BODY + 16)?,
        total,
    );
    let mut reply = Reply::new(request, status);
    reply.zeros(6).put(&[slots, channels.len() as u8]);
    let pointers = reply.here() as usize;
    reply.zeros(slots as usize * 2);
    reply.align4();
    let format = reply.here();
    reply.put(&format_block(persona, state.encoding_bits));

    for (i, &channel) in channels.iter().enumerate() {
        let name = match direction {
            Direction::Tx => persona.tx_channel_name(channel),
            Direction::Rx => persona.rx_channel_name(channel),
        };
        let name = reply.string(&name);
        reply.align4();
        reply.set_u16(pointers + i * 2, reply.here());
        let (opening, kind) = match direction {
            Direction::Tx => (0x1414, 0x0107),
            Direction::Rx => (0x141c, 0x000e),
        };
        reply.put_u16(opening).put_u16(channel).put_u16(0).put_u16(3).put_u16(channel);
        reply.put_u16(0).put_u16(0).put_u16(kind).put_u16(0).put_u16(0);
        reply.put_u16(name).put_u16(format).zeros(6).put_u16(name).zeros(8);
        if direction == Direction::Rx {
            reply.put(&hex("0608 0000 0000 0000 0000 0000 0202 0000"));
        }
    }
    Some(reply.finish())
}

/// `0x3000`: receive channels and what each is subscribed to, 16 to a page.
///
/// Body: `16 | n`, sixteen 20-byte slots, then the strings and one format
/// block. A slot is `number | 000e | format pointer | transmit channel name
/// | transmit device name | own name | state (4 bytes) | zero (4 bytes)`;
/// the two transmit pointers are zero for a channel with no subscription.
///
/// The strings are laid out in the order the Rio laid them out (per
/// channel: transmit channel, transmit device, own name; a transmit name
/// already written is pointed at again; the format block goes in when the
/// first channel reaches it). Nothing should depend on that order, but
/// matching it lets the capture replay compare whole replies.
fn rx_channels(request: &[u8], persona: &Persona, state: &ArcState) -> Option<Vec<u8>> {
    let start = start_channel(request)?.max(1);
    let last = persona.rx_channels.min(start.saturating_add(RX_PAGE - 1));
    let channels: Vec<u16> = (start..=last).collect();
    let status = if last < persona.rx_channels { STATUS_MORE } else { STATUS_OK };

    let mut reply = Reply::new(request, status);
    reply.put(&[RX_PAGE as u8, channels.len() as u8]);
    let records = reply.here() as usize;
    reply.zeros(RX_PAGE as usize * RX_RECORD);

    let mut tx_channel_names: Vec<(String, u16)> = Vec::new();
    let mut tx_device_names: Vec<(String, u16)> = Vec::new();
    let mut format: Option<u16> = None;

    fn intern(reply: &mut Reply, seen: &mut Vec<(String, u16)>, text: &str) -> u16 {
        if let Some((_, at)) = seen.iter().find(|(s, _)| s == text) {
            return *at;
        }
        let at = reply.string(text);
        seen.push((text.to_string(), at));
        at
    }

    for (i, &channel) in channels.iter().enumerate() {
        let slot = state.slot(channel);
        let (tx_channel, tx_device) = match slot {
            Some((s, _)) => (
                intern(&mut reply, &mut tx_channel_names, &s.tx_channel),
                intern(&mut reply, &mut tx_device_names, &s.tx_device),
            ),
            None => (0, 0),
        };
        let format = *format.get_or_insert_with(|| {
            reply.align4();
            let at = reply.here();
            reply.put(&format_block(persona, state.encoding_bits));
            at
        });
        let name = reply.string(&persona.rx_channel_name(channel));

        let at = records + i * RX_RECORD;
        reply.set_u16(at, channel);
        reply.set_u16(at + 2, 0x000e); // as captured
        reply.set_u16(at + 4, format);
        reply.set_u16(at + 6, tx_channel);
        reply.set_u16(at + 8, tx_device);
        reply.set_u16(at + 10, name);
        let state_word = slot.map_or(0, |(_, word)| *word);
        reply.bytes[at + 12..at + 16].copy_from_slice(&state_word.to_be_bytes());
    }

    // The Rio's first page ends with the strings of channel 17, a channel
    // the page has no slot for: it writes a channel's strings before it
    // finds the page full. Nothing points at them. They are reproduced so
    // that a whole reply can be compared with the device's.
    if last < persona.rx_channels {
        let next = last + 1;
        if let Some((s, _)) = state.slot(next) {
            intern(&mut reply, &mut tx_channel_names, &s.tx_channel);
            intern(&mut reply, &mut tx_device_names, &s.tx_device);
        }
        reply.string(&persona.rx_channel_name(next));
    }
    Some(reply.finish())
}

/// `0x3010`: subscribe receive channels.
///
/// Body, as the console sent it for one channel: `0000 | 01 | count`, then
/// per channel `number | transmit channel name pointer | transmit device
/// name pointer` and fourteen zero bytes, then the names.
///
/// The console only ever sent one channel per request, so the 20-byte
/// entry size is read off a single entry. A request for several is handled
/// on that assumption and has not been seen.
fn subscribe(request: &[u8], state: &mut ArcState) -> Option<()> {
    const ENTRY: usize = 20;
    let count = *request.get(REQUEST_BODY + 3)? as usize;
    for i in 0..count {
        let entry = REQUEST_BODY + 4 + i * ENTRY;
        let channel = u16_at(request, entry)?;
        let tx_channel = c_string(request, u16_at(request, entry + 2)? as usize)?;
        let tx_device = c_string(request, u16_at(request, entry + 4)? as usize)?;
        let slot = state.subscriptions.get_mut(channel.checked_sub(1)? as usize)?;
        *slot = Some((Subscription { tx_channel, tx_device }, RX_STATUS_UNRESOLVED));
    }
    Some(())
}

/// `0x3014`: unsubscribe receive channels. Body: `0000 | count`, then per
/// channel `0000 | number`.
fn unsubscribe(request: &[u8], state: &mut ArcState) -> Option<()> {
    let count = u16_at(request, REQUEST_BODY + 2)? as usize;
    for i in 0..count {
        let channel = u16_at(request, REQUEST_BODY + 6 + i * 4)?;
        if let Some(slot) = state.subscriptions.get_mut(channel.checked_sub(1)? as usize) {
            *slot = None;
        }
    }
    Some(())
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

    fn ask(request: &str, state: &mut ArcState) -> Vec<u8> {
        let (identity, persona) = rio();
        respond(&hex(request), &identity, &persona, state).expect("a reply")
    }

    // Requests and replies below are frames from the reference capture: the
    // QL1's request, then the Rio3224-D2's answer.

    #[test]
    fn channel_count_is_the_rios_answer() {
        let mut state = ArcState::new(&rio().1);
        assert_eq!(
            ask("2801000a001710000000", &mut state),
            hex("280100300017100000010ff9002000180000004000400020002000200001000100000000000000000000000000000000")
        );
    }

    #[test]
    fn device_info_is_the_rios_answer() {
        let mut state = ArcState::new(&rio().1);
        assert_eq!(
            ask("2801000a001910030000", &mut state),
            hex("2801008e001910030001001c001c0028007c008900000000000000000500003c005c003c000000000a0a\
                 000004030000280902041200100400000000593030342d59616d6168612d52696f333232342d44322d\
                 32356466303400000042524f4f4b4c594e332d32356466303400000000000000000000000000000000\
                 417564696e6174652044434d003a38303000")
        );
    }

    #[test]
    fn transmit_channels_are_the_rios_answer() {
        let mut state = ArcState::new(&rio().1);
        let reply = ask("28010010001a20000000000100010000", &mut state);
        assert_eq!(reply.len(), 380);
        assert_eq!(&reply[..10], &hex("2801017c001a20000001")[..]);
        // First record, the format block, the first and last names.
        assert_eq!(&reply[10..20], &hex("202000010107010c011c")[..]);
        assert_eq!(&reply[0x10c..0x11c], &hex("0000bb8001010018040000180018000c")[..]);
        assert_eq!(&reply[0x11c..0x11f], b"01\0");
        assert_eq!(&reply[reply.len() - 3..], b"32\0");
    }

    #[test]
    fn properties_read_back_as_the_rio_reported_them() {
        let mut state = ArcState::new(&rio().1);
        assert_eq!(
            ask(
                "2801002c002111000000001002018204820502100211830183028306031003110303802100f0806000220063",
                &mut state
            ),
            hex("280100740021110000011010020100018204004c82050050021000100211001083010054830200588306\
                 005c0310001003110002030300048021006000f00000806000700022000100630001000f4240000f4240\
                 000f4240028b0aab0003d09000000000000000000000000000000000ef450000")
        );
    }

    #[test]
    fn a_property_write_is_read_back_and_kept() {
        // The console sets latency to 0.25 ms, then reads everything again.
        let mut state = ArcState::new(&rio().1);
        assert_eq!(
            ask("28010028003311010000050482050020021100048301002403100004000000000003d0900003d090", &mut state),
            hex("2801002400331101000104048205001c0211000483010020031000040003d0900003d090")
        );
        assert_eq!(
            ask(
                "2801002c003711000000001002018204820502100211830183028306031003110303802100f0806000220063",
                &mut state
            ),
            hex("280100740037110000011010020100018204004c82050050021000100211000483010054830200588306\
                 005c0310000403110002030300048021006000f00000806000700022000100630001000f42400003d090\
                 0003d090028b0aab0003d09000000000000000000000000000000000ef450000")
        );
    }

    #[test]
    fn the_short_static_answers_are_the_rios() {
        let mut state = ArcState::new(&rio().1);
        assert_eq!(ask("28010010001b20320000000100010000", &mut state), hex("28010012001b203200010001000100207fff"));
        assert_eq!(ask("2801000a002233000000", &mut state), hex("280100120022330000013800397f398039ff"));
        assert_eq!(ask("2801001000341f110000000100010000", &mut state), hex("2801000a00341f110001"));
        assert_eq!(ask("28010010003532100000000100010000", &mut state), hex("2801000a003532100001"));
        assert_eq!(
            ask("28010010001f22000000000100010000", &mut state),
            hex("2801002c001f2200000110000000000000000000000000000000000000000000000000000000000000000000")
        );
        let labels = ask("28010010001e20100000000100010020", &mut state);
        assert_eq!(labels.len(), 268);
        assert_eq!(&labels[..12], &hex("2801010c001e201000012000")[..]);
        assert!(labels[12..].iter().all(|&b| b == 0));
        let list = ask("2801000a001811020000", &mut state);
        assert_eq!(list.len(), 140);
        assert_eq!(&list[..14], &hex("2801008c00181102000100208020")[..]);
    }

    #[test]
    fn unsubscribed_receive_channels_page_like_the_rios() {
        let mut state = ArcState::new(&rio().1);
        let first = ask("28010010001c30000000000100010000", &mut state);
        assert_eq!(u16_at(&first, 8), Some(STATUS_MORE), "24 channels do not fit one page");
        assert_eq!(&first[10..12], &[16, 16]);
        // Channel 1: no transmit pointers, a name, no state.
        let record = &first[12..32];
        assert_eq!(&record[..4], &hex("0001000e")[..]);
        assert_eq!(&record[6..10], &[0, 0, 0, 0]);
        let name = u16_at(record, 10).unwrap() as usize;
        assert_eq!(&first[name..name + 3], b"01\0");

        let second = ask("28010010001d30000000000100110000", &mut state);
        assert_eq!(u16_at(&second, 8), Some(STATUS_OK));
        assert_eq!(&second[10..12], &[16, 8], "channels 17 to 24");
        assert_eq!(&second[12..14], &[0, 17]);
    }

    #[test]
    fn a_subscription_is_stored_reported_unresolved_and_removable() {
        let mut state = ArcState::new(&rio().1);
        // The console patches its channel 19 to the Rio's output 1.
        assert_eq!(
            ask(
                "2801003a00443010000001010001002000230000000000000000000000000000\
                 313900593030312d59616d6168612d514c312d31376561326300",
                &mut state
            ),
            hex("2801000a004430100001")
        );
        assert_eq!(
            state.subscription(1),
            Some(&Subscription { tx_channel: "19".into(), tx_device: "Y001-Yamaha-QL1-17ea2c".into() })
        );

        let page = ask("28010010001c30000000000100010000", &mut state);
        let record = &page[12..32];
        let tx_channel = u16_at(record, 6).unwrap() as usize;
        let tx_device = u16_at(record, 8).unwrap() as usize;
        // The Rio put the first record's strings straight after the slots.
        assert_eq!(tx_channel, 0x14c);
        assert_eq!(tx_device, 0x14f);
        assert_eq!(u16_at(record, 4), Some(0x0168), "format block, 4-aligned after them");
        assert_eq!(c_string(&page, tx_channel).as_deref(), Some("19"));
        assert_eq!(c_string(&page, tx_device).as_deref(), Some("Y001-Yamaha-QL1-17ea2c"));
        assert_eq!(&record[12..16], &RX_STATUS_UNRESOLVED.to_be_bytes());

        assert_eq!(ask("28010010004330140000000100000001", &mut state), hex("2801000a004330140001"));
        assert_eq!(state.subscription(1), None);
    }

    // --- Dante Controller's questions. The requests and replies below are
    // Dante Controller 4 asking a DM3's Dante module (16 in, 16 out, 96 kHz,
    // latency 0.5 ms), from `yamaha-dm3/dm3-dante-cmc-arc.pcapng`. The
    // replies carry channel names and nothing that identifies the device.

    /// A device shaped like the DM3's module, for comparing layouts.
    fn dm3_shaped() -> (DeviceIdentity, Persona, ArcState) {
        let (identity, rio) = rio();
        let persona = Persona { tx_channels: 16, rx_channels: 16, sample_rate: 96_000, ..rio };
        let mut state = ArcState::new(&persona);
        state.restore_property(0x8205, &hex("0007a120"));
        (identity, persona, state)
    }

    fn ask_dm3(request: &str) -> Vec<u8> {
        let (identity, persona, mut state) = dm3_shaped();
        respond(&hex(request), &identity, &persona, &mut state).expect("a reply")
    }

    const DM3_2002: &str = "280801e00012200200010000000000000014003810100054007c009400ac00c400dc00f4010c0124013c0154016c0184\
        019c01b401cc0000050012001167000000000000060000100007a12000100002303100000a0000010050000000440068\
        00007fff000100000001770001010018040000180018000c303200000a000002007800000044006800007fff00020000\
        303300000a000003009000000044006800007fff00030000303400000a00000400a800000044006800007fff00040000\
        303500000a00000500c000000044006800007fff00050000303600000a00000600d800000044006800007fff00060000\
        303700000a00000700f000000044006800007fff00070000303800000a000008010800000044006800007fff00080000\
        303900000a000009012000000044006800007fff00090000313000000a00000a013800000044006800007fff000a0000\
        313100000a00000b015000000044006800007fff000b0000313200000a00000c016800000044006800007fff000c0000\
        313300000a00000d018000000044006800007fff000d0000313400000a00000e019800000044006800007fff000e0000\
        313500000a00000f01b000000044006800007fff000f0000313600000a00001001c800000044006800007fff00100000";

    /// The first channel and a half of the 772-byte transmit listing; the
    /// other fifteen records repeat the pattern.
    const DM3_2400_HEAD: &str = "2808030400032400000100000000000010100048007400a000cc00f801240150017c01a801d40200022c0258028402b0\
        02dc00000001770001010018040000180018000c30310000141400010000000300010000000001070000000000440034\
        0000000000000044000000000000000030320000141400020000000300020000000001070000000000700034";

    /// The first channel and a half of the 1028-byte receive listing.
    const DM3_3400_HEAD: &str = "2808040400143400000100000000000010100048008400c000fc0138017401b001ec0228026402a002dc031803540390\
        03cc00000001770001010018040000180018000c30310000141c000100000003000100000000000e0000000000440034\
        000000000000004400000000000000000608000000000000000000000202000030320000141c00020000000300020000";

    #[test]
    fn second_generation_transmit_channels_are_the_modules_answer() {
        assert_eq!(ask_dm3("28080010001220020000000100010010"), hex(DM3_2002));
    }

    #[test]
    fn the_full_channel_listings_are_laid_out_as_the_module_laid_them_out() {
        let tx = ask_dm3("28080022000324000000000000000000000100010001000000000000000000000000");
        assert_eq!(tx.len(), 772);
        assert_eq!(&tx[..hex(DM3_2400_HEAD).len()], &hex(DM3_2400_HEAD)[..]);
        // The last record: channel 16, its name four bytes before it.
        assert_eq!(&tx[0x2d8..0x2e0], &hex("3136000014140010")[..]);

        let rx = ask_dm3("28080022001434000000000000000000000100010001001000000000000000000000");
        assert_eq!(rx.len(), 1028);
        assert_eq!(&rx[..hex(DM3_3400_HEAD).len()], &hex(DM3_3400_HEAD)[..]);
        assert_eq!(&rx[rx.len() - 16..], &hex("06080000000000000000000002020000")[..]);
    }

    #[test]
    fn the_small_second_generation_answers_are_the_modules() {
        // Empty flow lists, both directions.
        let empty = format!("0000000000001000{}", "00".repeat(32));
        assert_eq!(
            ask_dm3("28080022001026000000000000000000000100010001001000000000000000000000"),
            hex(&format!("28080032001026000001{empty}"))
        );
        assert_eq!(
            ask_dm3("28080022001636000000000000000000000100010001001000000000000000000000"),
            hex(&format!("28080032001636000001{empty}"))
        );
        // 0x2204: answered in the older version, as the module answered it.
        assert_eq!(ask_dm3("28080010000822040000000100010000"), hex("2801001400082204000104000000000000000000"));
        // 0x2320: refused, with no body.
        assert_eq!(ask_dm3("2808000a000c23200000"), hex("2808000a000c23200030"));
        // Labels: sixteen empty slots when sixteen are asked for.
        let labels = ask_dm3("28080010000620100000000100010010");
        assert_eq!(labels.len(), 140);
        assert_eq!(&labels[..12], &hex("2808008c0006201000011000")[..]);
        // 0x2032 carries the transmit channel count.
        assert_eq!(ask_dm3("28080010000420320000000100010000"), hex("280800120004203200010001000100107fff"));
    }

    #[test]
    fn a_32_channel_device_pages_the_second_generation_listings() {
        let (identity, persona) = rio();
        let mut state = ArcState::new(&persona);
        let first =
            respond(&hex("28080010001220020000000100010010"), &identity, &persona, &mut state).unwrap();
        assert_eq!(u16_at(&first, 8), Some(STATUS_MORE), "32 inputs, 16 to a page");
        assert_eq!(&first[0x14..0x16], &[16, 16]);
        let second =
            respond(&hex("28080010001220020000000100110010"), &identity, &persona, &mut state).unwrap();
        assert_eq!(u16_at(&second, 8), Some(STATUS_OK));
        let record = u16_at(&second, 0x16).unwrap() as usize;
        assert_eq!(u16_at(&second, record + 2), Some(17), "the second page starts at channel 17");
        // The Rio's own latency, not the DM3's, in block B.
        assert_eq!(&second[0x48..0x4c], &hex("000f4240")[..]);

        // The receive listing stops at the Rio's 24 outputs.
        let mut request = hex("28080022001434000000000000000000000100010001001000000000000000000000");
        request[20..22].copy_from_slice(&17u16.to_be_bytes());
        let rx = respond(&request, &identity, &persona, &mut state).unwrap();
        assert_eq!(u16_at(&rx, 8), Some(STATUS_OK));
        assert_eq!(&rx[0x10..0x12], &[16, 8]);
    }

    #[test]
    fn a_read_with_no_list_returns_every_property_and_an_unknown_id_is_marked() {
        let (identity, persona) = rio();
        let mut state = ArcState::new(&persona);
        let all = respond(&hex("2808000a001811000000"), &identity, &persona, &mut state).unwrap();
        assert_eq!(&all[10..12], &[32, 32], "the 32 ids of the property list");
        assert_eq!(&all[12..14], &hex("8020")[..], "in the list's order");
        let rate = u16_at(&all, 14).unwrap() as usize;
        assert_eq!(&all[rate..rate + 4], &48_000u32.to_be_bytes(), "0x8020 is the sample rate");

        // Frame 73 of the DM3 capture asked for five ids the module did not
        // have; each came back as a zero id with the asked id as its value.
        let reply = respond(&hex("2808000e000a110000000002821800640000"), &identity, &persona, &mut state);
        assert_eq!(reply.unwrap()[10..], hex("0202 0000 8218 0000 0064")[..]);
    }

    #[test]
    fn the_older_protocol_version_gets_its_own_version_back() {
        // What this workspace's own client (and netaudio) sends.
        let mut state = ArcState::new(&rio().1);
        let reply = ask("27ff000a000110020000", &mut state);
        assert_eq!(&reply[..2], &[0x27, 0xff]);
        assert_eq!(&reply[10..reply.len() - 1], b"Y004-Yamaha-Rio3224-D2-25df04");
    }

    #[test]
    fn unknown_opcodes_and_noise_get_no_answer() {
        let (identity, persona) = rio();
        let mut state = ArcState::new(&persona);
        assert!(respond(&hex("2801000a00017777 0000"), &identity, &persona, &mut state).is_none());
        assert!(respond(&hex("ffff0020"), &identity, &persona, &mut state).is_none());
        assert!(respond(&[], &identity, &persona, &mut state).is_none());
    }

    #[test]
    fn a_request_cut_short_never_panics() {
        let (identity, persona) = rio();
        let mut state = ArcState::new(&persona);
        let whole = hex(
            "2801003a00443010000001010001002000230000000000000000000000000000\
             313900593030312d59616d6168612d514c312d31376561326300",
        );
        for cut in 0..whole.len() {
            let _ = respond(&whole[..cut], &identity, &persona, &mut state);
        }
        for opcode in [OP_PROPERTY_GET, OP_PROPERTY_SET, OP_TX_CHANNELS, OP_RX_CHANNELS, OP_UNSUBSCRIBE] {
            let mut request = hex("2801000a0001");
            request.extend_from_slice(&opcode.to_be_bytes());
            let _ = respond(&request, &identity, &persona, &mut state);
            request.extend_from_slice(&[0xff; 6]);
            let _ = respond(&request, &identity, &persona, &mut state);
        }
    }
}

//! Client for Audinate's Dante ARC control protocol (UDP, protocol id
//! `0x27FF`) - distinct from every vendor's preamp-control protocol this
//! project speaks elsewhere. Used only to *observe* current audio
//! routing (which RX channel is subscribed to which TX channel, on
//! which device) so `preamp-bridge init --infer-mappings` can guess
//! `[[mapping]]` entries from it. Never writes routing - the
//! subscription-add/remove opcodes exist on the wire but aren't
//! implemented here.
//!
//! Reimplemented (not vendored) against the wire format documented and
//! byte-tested by chris-ritsen/network-audio-controller's `netaudio-core`
//! Rust crate (Unlicense): <https://github.com/chris-ritsen/network-audio-controller>,
//! `packages/netaudio-core/src/{protocol,commands,parser,responses}.rs`.
//! Every constant, record layout, and the tests below are cross-checked
//! against that crate's own known-answer unit tests, not guessed.
//!
//! The ARC port itself is per-device, advertised via mDNS service
//! `_netaudio-arc._udp.local.` - see [`crate::arc_ports`], which filters
//! [`crate::discover`]'s output down to just that service (it also
//! browses `_netaudio-chan._udp`, whose port is a different thing
//! entirely).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::UdpSocket;
use tokio::time::timeout;

const PROTOCOL_ID: u16 = 0x27FF;
const OPCODE_DEVICE_NAME: u16 = 0x1002;
const OPCODE_TX_CHANNEL_INFO: u16 = 0x2000;
const OPCODE_TX_CHANNEL_NAMES: u16 = 0x2010;
const OPCODE_RX_CHANNELS: u16 = 0x3000;

const RESPONSE_HEADER_SIZE: usize = 10;
const BODY_HEADER_SIZE: usize = 2;
const RX_RECORD_SIZE: usize = 20;
const TX_RECORD_SIZE: usize = 8;
const TX_FRIENDLY_RECORD_SIZE: usize = 6;
const RX_CHANNELS_PER_PAGE: u16 = 16;
const TX_CHANNELS_PER_PAGE: u16 = 32;

/// Caps pagination at 128 channels (8 RX pages / 4 TX pages) rather than
/// querying a channel-count opcode first - one less request type, and
/// consistent with how a short/empty page already signals "no more
/// channels" structurally (see `parse_rx_page`/`parse_tx_info_page`).
const MAX_RX_PAGES: u16 = 8;
const MAX_TX_PAGES: u16 = 4;

const RX_RECORD_CHANNEL_NUMBER: usize = 0;
const RX_RECORD_TX_CHANNEL_POINTER: usize = 6;
const RX_RECORD_TX_DEVICE_POINTER: usize = 8;
const RX_RECORD_RX_CHANNEL_POINTER: usize = 10;

const TX_RECORD_CHANNEL_NUMBER: usize = 0;
const TX_RECORD_NAME_POINTER: usize = 6;

const TX_FRIENDLY_RECORD_CHANNEL_NUMBER: usize = 2;
const TX_FRIENDLY_RECORD_NAME_POINTER: usize = 4;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// One receive channel's current subscription, as reported by the
/// device that owns it. `tx_device_name` is `None` when the channel has
/// no live subscription - the wire format's `tx_device_pointer` is `0`
/// in that case, a structural fact rather than an interpreted status
/// code (this crate deliberately doesn't attempt to interpret the
/// separate `rx_status_code`/`subscription_status_code` fields
/// `netaudio-core` exposes raw and undocumented).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxChannel {
    pub number: u16,
    pub tx_channel_name: Option<String>,
    pub tx_device_name: Option<String>,
}

/// One transmit channel on a device, as needed to resolve an
/// [`RxChannel::tx_channel_name`] string back to a channel number.
/// `name` prefers the channel's friendly (user-assigned) name, falling
/// back to its raw name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxChannel {
    pub number: u16,
    pub name: Option<String>,
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn null_terminated_slice(data: &[u8], offset: usize) -> &[u8] {
    let end = data[offset..].iter().position(|&b| b == 0).map(|rel| offset + rel).unwrap_or(data.len());
    &data[offset..end]
}

fn string_at_pointer(data: &[u8], pointer: u16) -> Option<String> {
    if pointer == 0 || pointer as usize >= data.len() {
        return None;
    }
    std::str::from_utf8(null_terminated_slice(data, pointer as usize)).ok().map(str::to_owned)
}

fn build_packet(opcode: u16, payload: &[u8], transaction_id: u16) -> Vec<u8> {
    let length = 8 + payload.len();
    let mut packet = Vec::with_capacity(length);
    packet.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    packet.extend_from_slice(&(length as u16).to_be_bytes());
    packet.extend_from_slice(&transaction_id.to_be_bytes());
    packet.extend_from_slice(&opcode.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn channel_query_payload(starting_channel: u16) -> [u8; 8] {
    let mut payload = [0u8; 8];
    payload[3] = 0x01;
    payload[4..6].copy_from_slice(&starting_channel.to_be_bytes());
    payload
}

fn build_device_name_query(transaction_id: u16) -> Vec<u8> {
    build_packet(OPCODE_DEVICE_NAME, &[0x00, 0x00], transaction_id)
}

fn build_rx_query(page: u16, transaction_id: u16) -> Vec<u8> {
    let starting_channel = page * RX_CHANNELS_PER_PAGE + 1;
    build_packet(OPCODE_RX_CHANNELS, &channel_query_payload(starting_channel), transaction_id)
}

fn build_tx_query(page: u16, friendly_names: bool, transaction_id: u16) -> Vec<u8> {
    let opcode = if friendly_names { OPCODE_TX_CHANNEL_NAMES } else { OPCODE_TX_CHANNEL_INFO };
    let starting_channel = page * TX_CHANNELS_PER_PAGE + 1;
    build_packet(opcode, &channel_query_payload(starting_channel), transaction_id)
}

fn parse_device_name(response: &[u8]) -> Option<String> {
    if response.len() <= RESPONSE_HEADER_SIZE {
        return None;
    }
    let body = &response[RESPONSE_HEADER_SIZE..response.len() - 1];
    std::str::from_utf8(body).ok().map(str::to_owned)
}

fn page_records(response: &[u8], record_size: usize, records_per_page: u16) -> impl Iterator<Item = &[u8]> {
    let body = response.get(RESPONSE_HEADER_SIZE..).unwrap_or(&[]);
    (0..records_per_page as usize).map_while(move |index| {
        let offset = BODY_HEADER_SIZE + index * record_size;
        body.get(offset..offset + record_size)
    })
}

fn parse_rx_page(response: &[u8], starting_channel: u16) -> Vec<RxChannel> {
    let mut channels = Vec::new();
    for (index, record) in page_records(response, RX_RECORD_SIZE, RX_CHANNELS_PER_PAGE).enumerate() {
        let channel_number = u16_at(record, RX_RECORD_CHANNEL_NUMBER);
        let expected = starting_channel + index as u16;
        if channel_number == 0 || channel_number != expected {
            break;
        }

        let tx_channel_pointer = u16_at(record, RX_RECORD_TX_CHANNEL_POINTER);
        let tx_device_pointer = u16_at(record, RX_RECORD_TX_DEVICE_POINTER);
        let rx_channel_pointer = u16_at(record, RX_RECORD_RX_CHANNEL_POINTER);

        let rx_channel_name = string_at_pointer(response, rx_channel_pointer);
        let tx_device_name = string_at_pointer(response, tx_device_pointer);
        let tx_channel_name = if tx_channel_pointer != 0 {
            string_at_pointer(response, tx_channel_pointer)
        } else {
            rx_channel_name.clone()
        };

        channels.push(RxChannel {
            number: channel_number,
            tx_channel_name,
            tx_device_name,
        });
    }
    channels
}

fn parse_tx_friendly_page(response: &[u8]) -> Vec<(u16, String)> {
    let mut names = Vec::new();
    for record in page_records(response, TX_FRIENDLY_RECORD_SIZE, TX_CHANNELS_PER_PAGE) {
        let channel_number = u16_at(record, TX_FRIENDLY_RECORD_CHANNEL_NUMBER);
        if channel_number == 0 {
            break;
        }
        let name_pointer = u16_at(record, TX_FRIENDLY_RECORD_NAME_POINTER);
        if let Some(name) = string_at_pointer(response, name_pointer) {
            names.push((channel_number, name));
        }
    }
    names
}

fn parse_tx_info_page(response: &[u8], starting_channel: u16) -> Vec<TxChannel> {
    let mut channels = Vec::new();
    for (index, record) in page_records(response, TX_RECORD_SIZE, TX_CHANNELS_PER_PAGE).enumerate() {
        let channel_number = u16_at(record, TX_RECORD_CHANNEL_NUMBER);
        let expected = starting_channel + index as u16;
        if channel_number == 0 || channel_number != expected {
            break;
        }
        let name_pointer = u16_at(record, TX_RECORD_NAME_POINTER);
        channels.push(TxChannel {
            number: channel_number,
            name: string_at_pointer(response, name_pointer),
        });
    }
    channels
}

async fn request(addr: SocketAddr, packet: &[u8], transaction_id: u16) -> Result<Vec<u8>> {
    // A UDP socket can only connect() to a peer of the same address family
    // it was bound to - an IPv4 wildcard bind can't reach an IPv6 target
    // (the connect silently fails to route on some platforms rather than
    // erroring, which otherwise shows up as a request that hangs until
    // REQUEST_TIMEOUT instead of failing fast).
    let wildcard: SocketAddr = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
    let socket = UdpSocket::bind(wildcard).await.context("binding local UDP socket")?;
    socket.connect(addr).await.context("connecting to Dante ARC port")?;
    socket.send(packet).await.context("sending Dante ARC request")?;

    let mut buf = [0u8; 2048];
    let len = timeout(REQUEST_TIMEOUT, socket.recv(&mut buf))
        .await
        .context("timed out waiting for Dante ARC reply")?
        .context("receiving Dante ARC reply")?;

    let response = buf[..len].to_vec();
    if response.len() < 6 || u16_at(&response, 4) != transaction_id {
        bail!("Dante ARC reply was too short or had a mismatched transaction id");
    }
    Ok(response)
}

/// Queries a device's own Dante name over its ARC port.
pub async fn query_device_name(addr: SocketAddr) -> Result<Option<String>> {
    let transaction_id = 1;
    let packet = build_device_name_query(transaction_id);
    let response = request(addr, &packet, transaction_id).await?;
    Ok(parse_device_name(&response))
}

/// Queries every RX channel's current subscription over a device's ARC
/// port, paginating until a short page or the page cap is hit.
pub async fn query_rx_channels(addr: SocketAddr) -> Result<Vec<RxChannel>> {
    let mut channels = Vec::new();
    for page in 0..MAX_RX_PAGES {
        let transaction_id = page + 1;
        let packet = build_rx_query(page, transaction_id);
        let Ok(response) = request(addr, &packet, transaction_id).await else {
            break;
        };
        let starting_channel = page * RX_CHANNELS_PER_PAGE + 1;
        let parsed = parse_rx_page(&response, starting_channel);
        let complete = parsed.len() == RX_CHANNELS_PER_PAGE as usize;
        channels.extend(parsed);
        if !complete {
            break;
        }
    }
    Ok(channels)
}

/// Queries every TX channel's name over a device's ARC port, preferring
/// each channel's friendly (user-assigned) name where set.
pub async fn query_tx_channels(addr: SocketAddr) -> Result<Vec<TxChannel>> {
    let mut friendly = Vec::new();
    for page in 0..MAX_TX_PAGES {
        let transaction_id = page + 1;
        let packet = build_tx_query(page, true, transaction_id);
        let Ok(response) = request(addr, &packet, transaction_id).await else {
            break;
        };
        let parsed = parse_tx_friendly_page(&response);
        let complete = parsed.len() == TX_CHANNELS_PER_PAGE as usize;
        friendly.extend(parsed);
        if !complete {
            break;
        }
    }

    let mut channels = Vec::new();
    for page in 0..MAX_TX_PAGES {
        let transaction_id = MAX_TX_PAGES + page + 1;
        let packet = build_tx_query(page, false, transaction_id);
        let Ok(response) = request(addr, &packet, transaction_id).await else {
            break;
        };
        let starting_channel = page * TX_CHANNELS_PER_PAGE + 1;
        let parsed = parse_tx_info_page(&response, starting_channel);
        let complete = parsed.len() == TX_CHANNELS_PER_PAGE as usize;
        channels.extend(parsed);
        if !complete {
            break;
        }
    }

    for channel in &mut channels {
        if let Some((_, name)) = friendly.iter().find(|(number, _)| *number == channel.number) {
            channel.name = Some(name.clone());
        }
    }
    Ok(channels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket as TokioUdpSocket;

    // --- packet builders: known-answer bytes lifted from netaudio-core's
    // own byte-verified tests (protocol.rs/commands.rs), not guessed. ---

    #[test]
    fn device_name_query_matches_control_packet_framing() {
        // Same framing netaudio-core's `channel_count_builder_matches_python_layout`
        // verifies for a different opcode with the same 2-byte empty payload:
        // build_channel_count(1) == [0x27,0xFF, 0x00,0x0A, 0x00,0x01, 0x10,0x00, 0x00,0x00].
        let packet = build_device_name_query(1);
        assert_eq!(
            packet,
            [0x27, 0xFF, 0x00, 0x0A, 0x00, 0x01, 0x10, 0x02, 0x00, 0x00]
        );
    }

    #[test]
    fn rx_query_matches_known_bytes() {
        // netaudio-core's `receivers_builder_matches_python_layout`.
        let packet = build_rx_query(0, 0x1234);
        assert_eq!(
            packet,
            [0x27, 0xFF, 0x00, 0x10, 0x12, 0x34, 0x30, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn tx_query_selects_opcode_and_pages() {
        // netaudio-core's `transmitters_builder_selects_opcode_and_page`.
        let raw = build_tx_query(0, false, 0);
        assert_eq!(&raw[6..8], &OPCODE_TX_CHANNEL_INFO.to_be_bytes());
        let friendly = build_tx_query(1, true, 0);
        assert_eq!(&friendly[6..8], &OPCODE_TX_CHANNEL_NAMES.to_be_bytes());
        assert_eq!(&friendly[10..14], &[0x00, 0x01, 0x00, 33]);
    }

    // --- response parsers ---

    #[test]
    fn device_name_strips_header_and_terminator() {
        // netaudio-core's `device_name_strips_header_and_terminator`.
        let mut response = vec![0x27, 0xFF, 0x00, 0x16, 0x9e, 0x7f, 0x10, 0x02, 0x00, 0x01];
        response.extend_from_slice(b"avio-aes3-1\x00");
        assert_eq!(parse_device_name(&response).as_deref(), Some("avio-aes3-1"));
    }

    #[test]
    fn device_name_too_short_is_none() {
        assert_eq!(parse_device_name(&[0u8; 10]), None);
    }

    #[test]
    fn rx_page_decodes_a_live_subscription() {
        // netaudio-core's `rx_parser_decodes_subscription`.
        let mut response = vec![0u8; RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE + RX_RECORD_SIZE];
        let strings_base = response.len() as u16;
        response.extend_from_slice(b"rx-1\x00mix-hi\x00mixer\x00");

        let record = RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE;
        response[record..record + 2].copy_from_slice(&1u16.to_be_bytes());
        let tx_channel_pointer = strings_base + 5;
        let tx_device_pointer = strings_base + 12;
        let rx_channel_pointer = strings_base;
        response[record + 6..record + 8].copy_from_slice(&tx_channel_pointer.to_be_bytes());
        response[record + 8..record + 10].copy_from_slice(&tx_device_pointer.to_be_bytes());
        response[record + 10..record + 12].copy_from_slice(&rx_channel_pointer.to_be_bytes());

        let channels = parse_rx_page(&response, 1);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].number, 1);
        assert_eq!(channels[0].tx_channel_name.as_deref(), Some("mix-hi"));
        assert_eq!(channels[0].tx_device_name.as_deref(), Some("mixer"));
    }

    #[test]
    fn rx_page_unsubscribed_channel_has_no_tx_device_name() {
        // netaudio-core's `rx_parser_unsubscribed_falls_back_to_rx_name`:
        // tx_channel_pointer and tx_device_pointer both 0 means "not
        // subscribed" - a structural fact (a null pointer), not a status
        // code interpretation.
        let mut response = vec![0u8; RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE + RX_RECORD_SIZE];
        let strings_base = response.len() as u16;
        response.extend_from_slice(b"unused-1\x00");

        let record = RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE;
        response[record..record + 2].copy_from_slice(&1u16.to_be_bytes());
        response[record + 6..record + 8].copy_from_slice(&0u16.to_be_bytes());
        response[record + 8..record + 10].copy_from_slice(&0u16.to_be_bytes());
        response[record + 10..record + 12].copy_from_slice(&strings_base.to_be_bytes());

        let channels = parse_rx_page(&response, 1);
        assert_eq!(channels[0].tx_channel_name.as_deref(), Some("unused-1"));
        assert_eq!(channels[0].tx_device_name, None);
    }

    #[test]
    fn rx_page_stops_at_gap() {
        let response = vec![0u8; RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE + RX_RECORD_SIZE];
        assert!(parse_rx_page(&response, 1).is_empty());
    }

    #[test]
    fn tx_info_page_parses_names() {
        let mut response = vec![0u8; RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE + TX_RECORD_SIZE * 2];
        let strings_base = response.len() as u16;
        response.extend_from_slice(b"ch-1\x00ch-2\x00");

        let first = RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE;
        response[first..first + 2].copy_from_slice(&1u16.to_be_bytes());
        response[first + 6..first + 8].copy_from_slice(&strings_base.to_be_bytes());

        let second = first + TX_RECORD_SIZE;
        response[second..second + 2].copy_from_slice(&2u16.to_be_bytes());
        response[second + 6..second + 8].copy_from_slice(&(strings_base + 5).to_be_bytes());

        let channels = parse_tx_info_page(&response, 1);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].name.as_deref(), Some("ch-1"));
        assert_eq!(channels[1].name.as_deref(), Some("ch-2"));
    }

    #[test]
    fn tx_friendly_page_parses_names() {
        let mut response = vec![0u8; RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE + TX_FRIENDLY_RECORD_SIZE];
        let strings_base = response.len() as u16;
        response.extend_from_slice(b"Kick\x00");

        let record = RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE;
        response[record + 2..record + 4].copy_from_slice(&1u16.to_be_bytes());
        response[record + 4..record + 6].copy_from_slice(&strings_base.to_be_bytes());

        let names = parse_tx_friendly_page(&response);
        assert_eq!(names, vec![(1, "Kick".to_string())]);
    }

    // --- end-to-end against a mock UDP "device" ---

    #[tokio::test]
    async fn query_device_name_resolves_from_mock_reply() {
        let mock = TokioUdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (len, from) = mock.recv_from(&mut buf).await.unwrap();
            let transaction_id = u16_at(&buf[..len], 4);
            let mut reply = vec![0u8; RESPONSE_HEADER_SIZE];
            reply[0..2].copy_from_slice(&PROTOCOL_ID.to_be_bytes());
            reply[4..6].copy_from_slice(&transaction_id.to_be_bytes());
            reply[6..8].copy_from_slice(&OPCODE_DEVICE_NAME.to_be_bytes());
            reply.extend_from_slice(b"stagebox-1\x00");
            mock.send_to(&reply, from).await.unwrap();
        });

        let name = query_device_name(mock_addr).await.unwrap();
        assert_eq!(name.as_deref(), Some("stagebox-1"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn query_rx_channels_stops_after_short_page() {
        let mock = TokioUdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (len, from) = mock.recv_from(&mut buf).await.unwrap();
            let transaction_id = u16_at(&buf[..len], 4);

            let mut response = vec![0u8; RESPONSE_HEADER_SIZE];
            response[4..6].copy_from_slice(&transaction_id.to_be_bytes());
            response.extend_from_slice(&[0u8; BODY_HEADER_SIZE + RX_RECORD_SIZE]);
            let strings_base = response.len() as u16;
            response.extend_from_slice(b"in-1\x00tx-out-1\x00stagebox-1\x00");

            let record = RESPONSE_HEADER_SIZE + BODY_HEADER_SIZE;
            response[record..record + 2].copy_from_slice(&1u16.to_be_bytes());
            let tx_channel_pointer = strings_base + 5;
            let tx_device_pointer = strings_base + 14;
            let rx_channel_pointer = strings_base;
            response[record + 6..record + 8].copy_from_slice(&tx_channel_pointer.to_be_bytes());
            response[record + 8..record + 10].copy_from_slice(&tx_device_pointer.to_be_bytes());
            response[record + 10..record + 12].copy_from_slice(&rx_channel_pointer.to_be_bytes());

            mock.send_to(&response, from).await.unwrap();
        });

        let channels = query_rx_channels(mock_addr).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].number, 1);
        assert_eq!(channels[0].tx_channel_name.as_deref(), Some("tx-out-1"));
        assert_eq!(channels[0].tx_device_name.as_deref(), Some("stagebox-1"));
        server.await.unwrap();
    }
}

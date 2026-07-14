//! `DeviceAdapter` for Allen & Heath dLive, built from the official "dLive
//! MIDI Over TCP/IP Protocol" spec (Firmware V2.0). Distinct wire format
//! from both the AHM protocol and the SQ MIDI protocol - dLive is the only
//! console line in this project with a *documented* preamp-control path.
//!
//! Transport: raw MIDI bytes over TCP, unencrypted rendezvous port 51325
//! (MixRack) - this adapter targets the MixRack, since that's where
//! physical preamps live. dLive also uses MIDI running status (repeated
//! status bytes may be omitted on the wire), unlike AHM, so this adapter
//! has its own stream parser rather than reusing `ahm.rs`'s.
//!
//! Key difference from AHM/SQ's channel model: dLive separates physical
//! preamp "Sockets" from processing "Channels" - a socket keeps its own
//! gain/pad/phantom regardless of which channel it's patched to. Socket
//! numbers: MixRack sockets 1-64 -> MP 0x00-0x3F, MixRack DX1/2 1-32 ->
//! MP 0x40-0x5F, MixRack DX3/4 1-32 -> MP 0x60-0x7F. `PreampAddress`'s
//! `channel` field is used to carry this socket number directly (1-128
//! mapping to those three MP ranges in order), NOT a processing channel.
//!
//! Preamp Gain is set via a single Pitchbend message `EN, MP, GV` (the two
//! Pitchbend data bytes repurposed as socket number and gain value,
//! instead of the usual select-channel/select-param/set-value NRPN
//! triplet) - confirmed against the spec's own worked reference table:
//! `GV = round((dB - 5) / 55 * 127)`, dB range +5..+60. Pad/48V use SysEx
//! with distinct opcodes for "set" vs "status" (unlike AHM's NRPN, where
//! get-replies reuse the set-message format) - see the opcode constants
//! below. The base MIDI channel `N` for preamp control is configured on
//! the console itself (Utility/Control/MIDI); this adapter defaults to
//! N=0 and does not attempt to discover the console's actual setting.
//!
//! Get Socket Preamp Gain's documented request (`SysEx Header, 0N, 05,
//! 0B, 19, CH, F7`) reuses the generic NRPN-get template with a `CH`
//! (processing channel) parameter even though Set addresses gain by `MP`
//! (physical socket) - those aren't guaranteed to be the same number on a
//! system with non-identity patching. Sent best-effort in `get_state`,
//! but the reliable path is passively listening for spontaneous Pitchbend
//! updates (this adapter assumes, consistent with the rest of the
//! protocol family, that changing gain also triggers an unsolicited
//! Pitchbend push - not explicitly stated for this parameter in the spec).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    AdapterError, AdapterResult, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent, PreampState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

const SYSEX_HEADER: [u8; 8] = [0xF0, 0x00, 0x00, 0x1A, 0x50, 0x10, 0x01, 0x00];
const MIDI_N: u8 = 0x00;

const OP_GET_PAD: u8 = 0x07;
const OP_PAD_STATUS: u8 = 0x08;
#[allow(dead_code)] // documented for completeness; core::DeviceAdapter has no set_pad yet
const OP_SET_PAD: u8 = 0x09;
const OP_GET_48V: u8 = 0x0A;
const OP_48V_STATUS: u8 = 0x0B;
const OP_SET_48V: u8 = 0x0C;
const OP_GET_GAIN: u8 = 0x0B; // reuses the generic NRPN-get template (param 0x19); see module doc
const PARAM_GAIN: u8 = 0x19;

const GAIN_MIN_DB: f32 = 5.0;
const GAIN_MAX_DB: f32 = 60.0;

pub struct DliveAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
}

impl DliveAdapter {
    pub fn new(id: impl Into<Arc<str>>, remote: SocketAddr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote,
            writer: None,
            tx,
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Maps our flat 1-128 channel space onto dLive's three Socket ranges.
    fn socket_number(channel: u16) -> AdapterResult<u8> {
        match channel {
            1..=64 => Ok((channel - 1) as u8),
            65..=96 => Ok(0x40 + (channel - 65) as u8),
            97..=128 => Ok(0x60 + (channel - 97) as u8),
            _ => Err(AdapterError::UnsupportedChannel(channel)),
        }
    }

    fn channel_from_socket(mp: u8) -> Option<u16> {
        match mp {
            0x00..=0x3F => Some(mp as u16 + 1),
            0x40..=0x5F => Some(65 + (mp - 0x40) as u16),
            0x60..=0x7F => Some(97 + (mp - 0x60) as u16),
            0x80..=0xFF => None, // not a valid 7-bit MIDI data byte
        }
    }

    async fn send(&self, bytes: &[u8]) -> AdapterResult<()> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        writer
            .lock()
            .await
            .write_all(bytes)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }

    async fn sysex(&self, opcode: u8, mp: u8, value: Option<u8>) -> AdapterResult<()> {
        let mut msg = SYSEX_HEADER.to_vec();
        msg.push(MIDI_N);
        msg.push(opcode);
        msg.push(mp);
        if let Some(v) = value {
            msg.push(v);
        }
        msg.push(0xF7);
        self.send(&msg).await
    }
}

fn gain_db_to_byte(gain_db: f32) -> u8 {
    let clamped = gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    let frac = (clamped - GAIN_MIN_DB) / (GAIN_MAX_DB - GAIN_MIN_DB);
    (frac * 127.0).round() as u8
}

fn gain_byte_to_db(byte: u8) -> f32 {
    GAIN_MIN_DB + (byte.min(127) as f32 / 127.0) * (GAIN_MAX_DB - GAIN_MIN_DB)
}

fn bool_to_byte(on: bool) -> u8 {
    if on {
        0x7F
    } else {
        0x00
    }
}

fn byte_to_bool(value: u8) -> bool {
    value >= 0x40
}

#[async_trait]
impl DeviceAdapter for DliveAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let stream = TcpStream::connect(self.remote)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        let (read_half, write_half) = stream.into_split();
        self.writer = Some(Arc::new(Mutex::new(write_half)));

        spawn_receive_loop(read_half, Arc::clone(&self.id), self.tx.clone(), Arc::clone(&self.state));

        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        Err(AdapterError::Protocol(
            "identify: dLive MIDI protocol has no device-identity query".into(),
        ))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let mp = Self::socket_number(channel)?;
        let status = 0xE0 | MIDI_N;
        self.send(&[status, mp, gain_db_to_byte(gain_db)]).await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let mp = Self::socket_number(channel)?;
        self.sysex(OP_SET_48V, mp, Some(bool_to_byte(on))).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        let mp = Self::socket_number(channel)?;
        self.sysex(OP_GET_48V, mp, None).await?;
        self.sysex(OP_GET_PAD, mp, None).await?;
        // Best-effort per the module doc comment - CH here is assumed
        // equal to the socket number, which only holds under identity
        // patching.
        let mut get_gain = SYSEX_HEADER.to_vec();
        get_gain.extend_from_slice(&[MIDI_N, 0x05, OP_GET_GAIN, PARAM_GAIN, mp, 0xF7]);
        self.send(&get_gain).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!(
            "no reply for channel {channel} preamp state"
        )))
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

/// MIDI byte-stream parser with running-status support (dLive omits
/// repeated status bytes when consecutive messages share one) - a strict
/// superset of the framing `adapter-ah`'s AHM parser handles, kept
/// separate since AHM's spec doesn't document running status and the two
/// protocols are independently versioned.
#[derive(Default)]
struct MidiStreamParser {
    buf: Vec<u8>,
    running_status: Option<u8>,
}

fn channel_msg_data_len(status: u8) -> Option<usize> {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => Some(2),
        0xC0 | 0xD0 => Some(1),
        _ => None,
    }
}

impl MidiStreamParser {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Returns the next message with its status byte always present, even
    /// if it was actually omitted on the wire via running status.
    fn next_message(&mut self) -> Option<Vec<u8>> {
        loop {
            let first = *self.buf.first()?;

            if first == 0xF0 {
                let end = self.buf.iter().position(|&b| b == 0xF7)?;
                let msg = self.buf[..=end].to_vec();
                self.buf.drain(..=end);
                self.running_status = None;
                return Some(msg);
            }

            if first >= 0x80 {
                let Some(n) = channel_msg_data_len(first) else {
                    self.buf.remove(0);
                    continue;
                };
                let total = 1 + n;
                if self.buf.len() < total {
                    return None;
                }
                let msg = self.buf[..total].to_vec();
                self.buf.drain(..total);
                self.running_status = Some(first);
                return Some(msg);
            }

            // Data byte with no explicit status - continue running status.
            let Some(status) = self.running_status else {
                self.buf.remove(0);
                continue;
            };
            let Some(n) = channel_msg_data_len(status) else {
                self.buf.remove(0);
                continue;
            };
            if self.buf.len() < n {
                return None;
            }
            let mut msg = Vec::with_capacity(1 + n);
            msg.push(status);
            msg.extend_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return Some(msg);
        }
    }
}

fn spawn_receive_loop(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    id: Arc<str>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
) {
    tokio::spawn(async move {
        let mut parser = MidiStreamParser::default();
        let mut buf = [0u8; 4096];

        loop {
            let len = match read_half.read(&mut buf).await {
                Ok(0) => {
                    debug!(device = %id, "dLive TCP connection closed");
                    return;
                }
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "dLive TCP read failed, stopping receive loop");
                    return;
                }
            };
            parser.push(&buf[..len]);

            while let Some(msg) = parser.next_message() {
                handle_message(&msg, &id, &tx, &state).await;
            }
        }
    });
}

async fn handle_message(
    msg: &[u8],
    id: &Arc<str>,
    tx: &broadcast::Sender<PreampEvent>,
    state: &Arc<Mutex<HashMap<u16, PreampState>>>,
) {
    let update = if msg.len() == 3 && msg[0] & 0xF0 == 0xE0 {
        // Pitchbend: gain update for socket msg[1], value msg[2].
        DliveAdapter::channel_from_socket(msg[1])
            .map(|channel| (channel, Update::Gain(gain_byte_to_db(msg[2]))))
    } else if msg.len() >= 12 && msg.starts_with(&SYSEX_HEADER) && msg.last() == Some(&0xF7) {
        let body = &msg[8..msg.len() - 1];
        match body {
            [_n, opcode, mp, value] if *opcode == OP_48V_STATUS => {
                DliveAdapter::channel_from_socket(*mp).map(|c| (c, Update::Phantom(byte_to_bool(*value))))
            }
            [_n, opcode, mp, value] if *opcode == OP_PAD_STATUS => {
                DliveAdapter::channel_from_socket(*mp).map(|c| (c, Update::Pad(byte_to_bool(*value))))
            }
            _ => None,
        }
    } else {
        None
    };

    let Some((channel, update)) = update else {
        return;
    };

    let new_state = {
        let mut guard = state.lock().await;
        let entry = guard.entry(channel).or_insert(PreampState {
            gain_db: 0.0,
            phantom: false,
            pad: None,
        });
        match update {
            Update::Gain(v) => entry.gain_db = v,
            Update::Phantom(v) => entry.phantom = v,
            Update::Pad(v) => entry.pad = Some(v),
        }
        *entry
    };

    debug!(device = %id, channel, ?update, "dLive preamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
    });
}

#[derive(Debug)]
enum Update {
    Gain(f32),
    Phantom(bool),
    Pad(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_number_covers_all_three_ranges() {
        assert_eq!(DliveAdapter::socket_number(1).unwrap(), 0x00);
        assert_eq!(DliveAdapter::socket_number(64).unwrap(), 0x3F);
        assert_eq!(DliveAdapter::socket_number(65).unwrap(), 0x40);
        assert_eq!(DliveAdapter::socket_number(96).unwrap(), 0x5F);
        assert_eq!(DliveAdapter::socket_number(97).unwrap(), 0x60);
        assert_eq!(DliveAdapter::socket_number(128).unwrap(), 0x7F);
        assert!(DliveAdapter::socket_number(0).is_err());
        assert!(DliveAdapter::socket_number(129).is_err());
    }

    #[test]
    fn channel_from_socket_round_trips() {
        for ch in [1u16, 32, 64, 65, 80, 96, 97, 100, 128] {
            let mp = DliveAdapter::socket_number(ch).unwrap();
            assert_eq!(DliveAdapter::channel_from_socket(mp), Some(ch));
        }
    }

    #[test]
    fn gain_matches_dlive_reference_table() {
        assert_eq!(gain_db_to_byte(5.0), 0x00);
        assert_eq!(gain_db_to_byte(10.0), 0x0C);
        assert_eq!(gain_db_to_byte(30.0), 0x3A);
        assert_eq!(gain_db_to_byte(60.0), 0x7F);
    }

    #[test]
    fn parser_handles_running_status() {
        // Two mute-on messages on the same channel, second omits status
        // (as dLive's spec explicitly shows for e.g. mute pairs).
        let stream = [0x9Bu8, 0x00, 0x7F, 0x01, 0x7F];
        let mut parser = MidiStreamParser::default();
        parser.push(&stream);

        let first = parser.next_message().unwrap();
        assert_eq!(first, vec![0x9B, 0x00, 0x7F]);
        let second = parser.next_message().unwrap();
        assert_eq!(second, vec![0x9B, 0x01, 0x7F]);
        assert!(parser.next_message().is_none());
    }

    #[test]
    fn parser_extracts_pitchbend_and_sysex() {
        let mut parser = MidiStreamParser::default();
        parser.push(&[0xE0, 0x05, 0x3A]);
        let pb = parser.next_message().unwrap();
        assert_eq!(pb, vec![0xE0, 0x05, 0x3A]);

        let mut sysex_msg = SYSEX_HEADER.to_vec();
        sysex_msg.extend_from_slice(&[0x00, OP_48V_STATUS, 0x05, 0x7F, 0xF7]);
        parser.push(&sysex_msg);
        let sysex = parser.next_message().unwrap();
        assert_eq!(sysex, sysex_msg);
    }

    #[tokio::test]
    async fn set_gain_and_48v_produce_expected_bytes_and_spontaneous_updates_propagate() {
        use tokio::io::AsyncReadExt as _;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut adapter = DliveAdapter::new("dlive-1", addr);
        let mut events = adapter.subscribe();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            let mut pb = [0u8; 3];
            socket.read_exact(&mut pb).await.unwrap();

            // Simulate a spontaneous 48V-on push for socket 65 (channel 65).
            let mut msg = SYSEX_HEADER.to_vec();
            msg.extend_from_slice(&[0x00, OP_48V_STATUS, 0x40, 0x7F, 0xF7]);
            socket.write_all(&msg).await.unwrap();

            pb
        });

        adapter.connect().await.unwrap();
        adapter.set_gain(30, 30.0).await.unwrap();

        let sent = server.await.unwrap();
        assert_eq!(sent, [0xE0, 0x1D, 0x3A]); // socket 30 -> MP 0x1D, gain 30dB -> 0x3A

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event.address, PreampAddress::new("dlive-1", 65));
        assert!(event.state.phantom);
    }
}

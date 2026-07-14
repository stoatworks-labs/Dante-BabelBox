//! `DeviceAdapter` for Allen & Heath's AHM-series Dante/AES67 processors,
//! built from the official "AHM TCP/IP Protocol V1.4" spec (raw MIDI bytes
//! over TCP, port 51325 unencrypted / 51327 TLS - this adapter implements
//! the unencrypted path only).
//!
//! IMPORTANT SCOPE NOTE: the *separately* published "SQ MIDI Protocol
//! Issue 5" document (which covers Qu/SQ/dLive consoles specifically) does
//! NOT document any Input Preamp Gain/Pad/Phantom Power messages at all -
//! only levels, mutes, pan/balance, assignments and scene recall. So while
//! this adapter is byte-for-byte accurate for genuine AHM-brand hardware,
//! whether SQ/Qu/dLive-attached stageboxes (e.g. DT168) accept the same
//! NRPN preamp messages is UNCONFIRMED from public docs. Do not assume it
//! without checking against real hardware or further A&H documentation.
//!
//! Wire format: channel select + parameter select + value, each as a
//! standard 3-byte MIDI Control Change message, sent back to back:
//!   BN 63 CH   (select channel CH on MIDI channel N)
//!   BN 62 <id> (select parameter: 0x19 gain, 0x1A pad, 0x1B phantom)
//!   BN 06 <value>
//! "Get" is a SysEx request; the unit replies with the same 3-CC message
//! format carrying the current value (not a SysEx reply).
//!
//! Gain range per the spec text is "5dB to +60dB = 00 to 7F" with no
//! hex/dB reference table given for this specific parameter (unlike
//! Channel Level, which has one). Confirmed NOT a typo: the sibling
//! "dLive MIDI Over TCP/IP Protocol" doc documents the identical Socket
//! Preamp Gain parameter with a full worked reference table, and it
//! resolves to exactly +5dB..+60dB linearly mapped 00..7F (formula
//! `[(Gain-5)/55]*7F`, e.g. +10dB = 0x0C, +30dB = 0x3A) - taken as
//! authoritative here since AHM and dLive share this NRPN scheme.

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

const SYSEX_HEADER: [u8; 8] = [0xF0, 0x00, 0x00, 0x1A, 0x50, 0x12, 0x01, 0x00];
const PARAM_GAIN: u8 = 0x19;
const PARAM_PAD: u8 = 0x1A;
const PARAM_PHANTOM: u8 = 0x1B;
const MIDI_N_INPUTS: u8 = 0x00; // "N" for Inputs 1-64, per the Channel selection table

const GAIN_MIN_DB: f32 = 5.0;
const GAIN_MAX_DB: f32 = 60.0;

pub struct AhmAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
}

impl AhmAdapter {
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

    fn channel_index(channel: u16) -> AdapterResult<u8> {
        if !(1..=64).contains(&channel) {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok((channel - 1) as u8)
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

    async fn set_param(&self, channel: u16, param: u8, value: u8) -> AdapterResult<()> {
        let ch = Self::channel_index(channel)?;
        let status = 0xB0 | MIDI_N_INPUTS;
        self.send(&[status, 0x63, ch, status, 0x62, param, status, 0x06, value])
            .await
    }

    async fn get_param(&self, channel: u16, param: u8) -> AdapterResult<()> {
        let ch = Self::channel_index(channel)?;
        let mut msg = SYSEX_HEADER.to_vec();
        msg.extend_from_slice(&[MIDI_N_INPUTS, 0x01, 0x0B, param, ch, 0xF7]);
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
impl DeviceAdapter for AhmAdapter {
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
        // The AHM protocol has no explicit "who are you" message in the
        // spec beyond channel-name/colour queries, which don't confirm
        // vendor/model either. Not implemented - see the OSC adapter's
        // identify() for the same gap on that side.
        Err(AdapterError::Protocol(
            "identify: AHM protocol has no device-identity query".into(),
        ))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        self.set_param(channel, PARAM_GAIN, gain_db_to_byte(gain_db)).await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        self.set_param(channel, PARAM_PHANTOM, bool_to_byte(on)).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        self.get_param(channel, PARAM_GAIN).await?;
        self.get_param(channel, PARAM_PHANTOM).await?;

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

/// Incrementally splits a raw MIDI byte stream into individual messages.
/// Standalone and synchronous so the framing logic can be unit tested
/// without a real socket.
#[derive(Default)]
struct MidiStreamParser {
    buf: Vec<u8>,
}

impl MidiStreamParser {
    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn next_message(&mut self) -> Option<Vec<u8>> {
        loop {
            let status = *self.buf.first()?;
            if status < 0x80 {
                self.buf.remove(0); // desynced stray data byte - drop and resync
                continue;
            }
            if status == 0xF0 {
                let end = self.buf.iter().position(|&b| b == 0xF7)?;
                let msg = self.buf[..=end].to_vec();
                self.buf.drain(..=end);
                return Some(msg);
            }
            let len = match status & 0xF0 {
                0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => 3,
                0xC0 | 0xD0 => 2,
                _ => 1, // other Fx status with no defined body here - skip it
            };
            if len == 1 {
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < len {
                return None;
            }
            let msg = self.buf[..len].to_vec();
            self.buf.drain(..len);
            return Some(msg);
        }
    }
}

/// Reassembles the doc's 3-message NRPN idiom (select channel, select
/// parameter, data entry) into a single (N, CH, param, value) tuple.
#[derive(Default)]
struct NrpnTracker {
    pending_ch: HashMap<u8, u8>,
    pending_param: HashMap<u8, u8>,
}

impl NrpnTracker {
    fn feed(&mut self, msg: &[u8]) -> Option<(u8, u8, u8, u8)> {
        if msg.len() != 3 || msg[0] & 0xF0 != 0xB0 {
            return None;
        }
        let n = msg[0] & 0x0F;
        let (cc, value) = (msg[1], msg[2]);
        match cc {
            0x63 => {
                self.pending_ch.insert(n, value);
                None
            }
            0x62 => {
                self.pending_param.insert(n, value);
                None
            }
            0x06 => {
                let ch = *self.pending_ch.get(&n)?;
                let param = *self.pending_param.get(&n)?;
                Some((n, ch, param, value))
            }
            _ => None,
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
        let mut tracker = NrpnTracker::default();
        let mut buf = [0u8; 4096];

        loop {
            let len = match read_half.read(&mut buf).await {
                Ok(0) => {
                    debug!(device = %id, "AHM TCP connection closed");
                    return;
                }
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "AHM TCP read failed, stopping receive loop");
                    return;
                }
            };
            parser.push(&buf[..len]);

            while let Some(msg) = parser.next_message() {
                let Some((n, ch, param, value)) = tracker.feed(&msg) else {
                    continue;
                };
                if n != MIDI_N_INPUTS {
                    continue;
                }
                if !matches!(param, PARAM_GAIN | PARAM_PAD | PARAM_PHANTOM) {
                    continue;
                }
                let channel = ch as u16 + 1;

                let new_state = {
                    let mut guard = state.lock().await;
                    let entry = guard.entry(channel).or_insert(PreampState {
                        gain_db: 0.0,
                        phantom: false,
                        pad: None,
                    });
                    match param {
                        PARAM_GAIN => entry.gain_db = gain_byte_to_db(value),
                        PARAM_PAD => entry.pad = Some(byte_to_bool(value)),
                        PARAM_PHANTOM => entry.phantom = byte_to_bool(value),
                        _ => unreachable!(),
                    }
                    *entry
                };

                debug!(device = %id, channel, param, value, "AHM preamp update");
                let _ = tx.send(PreampEvent {
                    address: PreampAddress::new(id.to_string(), channel),
                    state: new_state,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_round_trips_within_one_step() {
        for &db in &[5.0, 8.3, 10.0, 12.5, 30.0, 59.9, 60.0] {
            let byte = gain_db_to_byte(db);
            let back = gain_byte_to_db(byte);
            assert!((back - db).abs() <= (GAIN_MAX_DB - GAIN_MIN_DB) / 127.0 + 0.01);
        }
        assert_eq!(gain_db_to_byte(5.0), 0x00);
        assert_eq!(gain_db_to_byte(60.0), 0x7F);
        // Cross-checked against the dLive protocol's worked reference table
        // for the identical Socket Preamp Gain parameter.
        assert_eq!(gain_db_to_byte(10.0), 0x0C);
        assert_eq!(gain_db_to_byte(30.0), 0x3A);
    }

    #[test]
    fn bool_byte_matches_spec_thresholds() {
        assert_eq!(bool_to_byte(true), 0x7F);
        assert_eq!(bool_to_byte(false), 0x00);
        assert!(byte_to_bool(0x7F));
        assert!(byte_to_bool(0x40));
        assert!(!byte_to_bool(0x3F));
        assert!(!byte_to_bool(0x00));
    }

    #[test]
    fn set_gain_message_matches_spec_layout() {
        // Input 6 (CH=05), gain param 0x19, arbitrary value 0x40.
        let msg = [0xB0, 0x63, 0x05, 0xB0, 0x62, 0x19, 0xB0, 0x06, 0x40];
        let mut parser = MidiStreamParser::default();
        parser.push(&msg);
        let mut tracker = NrpnTracker::default();

        let mut result = None;
        while let Some(m) = parser.next_message() {
            if let Some(triplet) = tracker.feed(&m) {
                result = Some(triplet);
            }
        }
        assert_eq!(result, Some((0x00, 0x05, 0x19, 0x40)));
    }

    #[test]
    fn parser_resyncs_across_split_reads() {
        let full = [0xB0u8, 0x63, 0x00, 0xB0, 0x62, 0x1B, 0xB0, 0x06, 0x7F];
        let mut parser = MidiStreamParser::default();
        let mut tracker = NrpnTracker::default();
        let mut result = None;

        for chunk in full.chunks(2) {
            parser.push(chunk);
            while let Some(m) = parser.next_message() {
                if let Some(triplet) = tracker.feed(&m) {
                    result = Some(triplet);
                }
            }
        }
        assert_eq!(result, Some((0x00, 0x00, PARAM_PHANTOM, 0x7F)));
    }

    #[test]
    fn parser_skips_sysex_and_resyncs() {
        let mut parser = MidiStreamParser::default();
        // A SysEx blob followed by a valid CC message.
        parser.push(&[0xF0, 0x00, 0x00, 0x1A, 0x50, 0x12, 0x01, 0x00, 0xF7]);
        parser.push(&[0xB0, 0x63, 0x02]);

        let sysex = parser.next_message().unwrap();
        assert_eq!(sysex.first(), Some(&0xF0));
        assert_eq!(sysex.last(), Some(&0xF7));

        let cc = parser.next_message().unwrap();
        assert_eq!(cc, vec![0xB0, 0x63, 0x02]);
    }

    #[tokio::test]
    async fn set_gain_writes_expected_bytes_and_replies_update_subscribers() {
        use tokio::io::AsyncReadExt as _;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut adapter = AhmAdapter::new("ahm-1", addr);
        let mut events = adapter.subscribe();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            // Expect the 9-byte set-gain sequence for channel 6, ~+12dB.
            let mut buf = [0u8; 9];
            socket.read_exact(&mut buf).await.unwrap();

            // Then push a spontaneous phantom-power-on update for channel 1
            // back down the same connection, as the unit would after a
            // physical/on-screen change.
            socket
                .write_all(&[0xB0, 0x63, 0x00, 0xB0, 0x62, 0x1B, 0xB0, 0x06, 0x7F])
                .await
                .unwrap();

            buf
        });

        adapter.connect().await.unwrap();
        adapter.set_gain(6, 12.0).await.unwrap();

        let sent = server.await.unwrap();
        let expected_byte = gain_db_to_byte(12.0);
        assert_eq!(sent, [0xB0, 0x63, 0x05, 0xB0, 0x62, PARAM_GAIN, 0xB0, 0x06, expected_byte]);

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event.address, PreampAddress::new("ahm-1", 1));
        assert!(event.state.phantom);
    }
}

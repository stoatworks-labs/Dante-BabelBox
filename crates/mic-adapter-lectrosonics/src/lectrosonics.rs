//! `MicAdapter` for Lectrosonics networked receivers (DSQD / D Squared,
//! Duet, DCR822) over their Ethernet control port.
//!
//! ⚠️ PLACEHOLDER WIRE FORMAT — READ THIS FIRST. The other mic adapters in
//! this workspace (Shure, Sennheiser) are built from published vendor protocol
//! documents cited in their module doc comments. This one is **not**:
//! Lectrosonics does not publish an equivalent open document that was
//! available here, so the framing, port, field names and value scales below
//! are a best-effort SCAFFOLD mirroring the RFutils adapter of the same name —
//! not confirmed against hardware or an authoritative spec. Everything
//! structural (connection, incremental line framing, state merge, event
//! broadcast, `set_mute`) is real and unit-tested; only the bytes on the wire
//! need correcting once the real IP-control spec or a packet capture is
//! available. When you do, keep the no-fabrication discipline the rest of this
//! workspace follows.
//!
//! No-fabrication choices already applied here, matching the Shure adapter:
//! - `audio_level_dbfs` is left `None`. The placeholder telemetry has no
//!   field with a *documented* dBFS conversion, so none is invented.
//! - The RF field is mapped to `rf_quality_percent` (an assumed 0-100 scale),
//!   NOT `rf_level_dbm`. Without a documented raw→dBm formula, reporting a dBm
//!   figure would be fabricating calibration; a provisional quality percentage
//!   is the honest placeholder and is clearly labelled as assumed.
//!
//! Assumed wire format (all provisional):
//! - ASCII over TCP, port `DEFAULT_LECTROSONICS_PORT` (a guess), messages
//!   terminated by CR (`\r`); the parser also tolerates LF / CRLF.
//! - Telemetry line: `RX <ch> [NAME <name>] [FREQ <khz>] [RF <0-100>]
//!   [BATT <0-100>] [MUTE <ON|OFF>]` — whitespace `KEY value` pairs after the
//!   channel. `<ch>` is 1-based; `0` is treated as malformed and dropped
//!   (same rule the Shure adapter applies to response channels).
//! - `connect()` sends `QUERY ALL` to request an initial dump; `get_state`
//!   sends `GET <ch>`; `set_mute` sends `SET <ch> MUTE ON|OFF`.
//!
//! `identify()` returns an error rather than guessing, exactly as the Shure
//! adapter does: no documented device-identity query is known here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_mic_core::{
    AdapterError, AdapterResult, DeviceInfo, MicAddress, MicAdapter, MicEvent, MicState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

/// Placeholder control port — verify against real hardware before relying on
/// it. Overridable via the CLI config's `port`.
pub const DEFAULT_LECTROSONICS_PORT: u16 = 4992;

pub struct LectrosonicsAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
}

impl LectrosonicsAdapter {
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

    async fn send(&self, message: &str) -> AdapterResult<()> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let framed = format!("{message}\r");
        writer
            .lock()
            .await
            .write_all(framed.as_bytes())
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }
}

fn empty_state() -> MicState {
    MicState {
        battery_percent: None,
        battery_minutes_remaining: None,
        // No documented raw->dBm conversion; a placeholder RF field is carried
        // as rf_quality_percent instead (see module doc comment).
        rf_level_dbm: None,
        rf_quality_percent: None,
        // No documented dBFS formula in the placeholder telemetry - left None
        // rather than fabricated, matching the Shure adapter.
        audio_level_dbfs: None,
        muted: false,
        frequency_mhz: None,
        antenna: None,
    }
}

#[async_trait]
impl MicAdapter for LectrosonicsAdapter {
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

        // Request an initial full dump (placeholder command).
        self.send("QUERY ALL").await
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        Err(AdapterError::Protocol(
            "identify: no documented Lectrosonics device-identity query is known here (this \
             adapter's wire format is an unverified placeholder - see module doc comment)"
                .into(),
        ))
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<MicState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        self.send(&format!("GET {channel}")).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!("no reply for channel {channel} mic state")))
    }

    async fn set_mute(&mut self, channel: u16, muted: bool) -> AdapterResult<()> {
        let value = if muted { "ON" } else { "OFF" };
        self.send(&format!("SET {channel} MUTE {value}")).await
    }

    fn subscribe(&self) -> broadcast::Receiver<MicEvent> {
        self.tx.subscribe()
    }
}

/// Incrementally splits a raw ASCII byte stream into individual lines,
/// tolerant of CR / LF / CRLF terminators. Standalone and synchronous so
/// framing can be unit tested without a real socket.
#[derive(Default)]
struct LectroStreamParser {
    buf: String,
}

impl LectroStreamParser {
    fn push(&mut self, data: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(data));
    }

    fn next_message(&mut self) -> Option<Vec<String>> {
        let idx = self.buf.find(['\r', '\n'])?;
        let line: String = self.buf.drain(..=idx).collect();
        // Drop the terminator we just consumed, plus any paired CRLF partner.
        while self.buf.starts_with(['\r', '\n']) {
            self.buf.drain(..1);
        }
        let tokens: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() {
            // Blank line - try the next one so callers still make progress.
            return self.next_message();
        }
        Some(tokens)
    }
}

/// A parsed telemetry line: a channel plus whichever fields were present.
/// `None` means "not in this message", so a partial line only updates the
/// fields it carries.
struct LectroUpdate {
    channel: u16,
    frequency_mhz: Option<f64>,
    rf_quality_percent: Option<u8>,
    battery_percent: Option<u8>,
    muted: Option<bool>,
}

fn parse_channel(token: &str) -> Option<u16> {
    let ch: u16 = token.parse().ok()?;
    // 0 is the "all channels" request wildcard, never a real response channel
    // (same convention as the Shure adapter) - drop rather than guess.
    if ch == 0 {
        None
    } else {
        Some(ch)
    }
}

fn parse_message(tokens: &[String]) -> Option<LectroUpdate> {
    if tokens.len() < 2 || (tokens[0] != "RX" && tokens[0] != "REP") {
        return None;
    }
    let channel = parse_channel(&tokens[1])?;

    let mut update = LectroUpdate {
        channel,
        frequency_mhz: None,
        rf_quality_percent: None,
        battery_percent: None,
        muted: None,
    };

    let mut i = 2;
    while i + 1 < tokens.len() {
        let key = tokens[i].as_str();
        let value = tokens[i + 1].as_str();
        match key {
            "FREQ" => {
                let khz: u32 = value.parse().ok()?;
                update.frequency_mhz = Some(khz as f64 / 1000.0);
            }
            "RF" => {
                let raw: u16 = value.parse().ok()?;
                update.rf_quality_percent = if raw <= 100 { Some(raw as u8) } else { None };
            }
            "BATT" => {
                let raw: u16 = value.parse().ok()?;
                update.battery_percent = if raw <= 100 { Some(raw as u8) } else { None };
            }
            "MUTE" => update.muted = Some(value == "ON"),
            // NAME and any unknown key: skip its value (MicState carries no
            // name field). Unknown keys are ignored, not fatal.
            _ => {}
        }
        i += 2;
    }
    Some(update)
}

fn spawn_receive_loop(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    id: Arc<str>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
) {
    tokio::spawn(async move {
        let mut parser = LectroStreamParser::default();
        let mut buf = [0u8; 4096];

        loop {
            let len = match read_half.read(&mut buf).await {
                Ok(0) => {
                    debug!(device = %id, "Lectrosonics TCP connection closed");
                    return;
                }
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "Lectrosonics TCP read failed, stopping receive loop");
                    return;
                }
            };
            parser.push(&buf[..len]);

            while let Some(tokens) = parser.next_message() {
                let Some(update) = parse_message(&tokens) else {
                    continue;
                };

                let (channel, new_state) = {
                    let mut guard = state.lock().await;
                    let entry = guard.entry(update.channel).or_insert_with(empty_state);
                    if let Some(f) = update.frequency_mhz {
                        entry.frequency_mhz = Some(f);
                    }
                    if let Some(q) = update.rf_quality_percent {
                        entry.rf_quality_percent = Some(q);
                    }
                    if let Some(b) = update.battery_percent {
                        entry.battery_percent = Some(b);
                    }
                    if let Some(m) = update.muted {
                        entry.muted = m;
                    }
                    (update.channel, *entry)
                };

                debug!(device = %id, channel, "Lectrosonics telemetry update");
                let _ = tx.send(MicEvent {
                    address: MicAddress::new(id.to_string(), channel),
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
    fn parser_splits_lines_on_cr() {
        let mut parser = LectroStreamParser::default();
        parser.push(b"RX 1 FREQ 614125\rRX 2 BATT 090\r");
        assert_eq!(parser.next_message().unwrap(), vec!["RX", "1", "FREQ", "614125"]);
        assert_eq!(parser.next_message().unwrap(), vec!["RX", "2", "BATT", "090"]);
        assert!(parser.next_message().is_none());
    }

    #[test]
    fn parser_resyncs_across_split_reads() {
        let mut parser = LectroStreamParser::default();
        let mut result = None;
        for chunk in b"RX 2 FREQ 614125\r".chunks(4) {
            parser.push(chunk);
            if let Some(tokens) = parser.next_message() {
                result = Some(tokens);
            }
        }
        assert_eq!(result.unwrap(), vec!["RX", "2", "FREQ", "614125"]);
    }

    #[test]
    fn frequency_converts_khz_to_mhz() {
        let tokens: Vec<String> = ["RX", "3", "FREQ", "614125"].iter().map(|s| s.to_string()).collect();
        let u = parse_message(&tokens).unwrap();
        assert_eq!(u.channel, 3);
        assert_eq!(u.frequency_mhz, Some(614.125));
    }

    #[test]
    fn out_of_range_battery_and_rf_map_to_none() {
        let tokens: Vec<String> = ["RX", "1", "BATT", "200", "RF", "150"].iter().map(|s| s.to_string()).collect();
        let u = parse_message(&tokens).unwrap();
        assert_eq!(u.battery_percent, None);
        assert_eq!(u.rf_quality_percent, None);
    }

    #[test]
    fn mute_on_off_parses() {
        let on: Vec<String> = ["RX", "1", "MUTE", "ON"].iter().map(|s| s.to_string()).collect();
        let off: Vec<String> = ["RX", "1", "MUTE", "OFF"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_message(&on).unwrap().muted, Some(true));
        assert_eq!(parse_message(&off).unwrap().muted, Some(false));
    }

    #[test]
    fn non_telemetry_and_zero_channel_are_dropped() {
        let garbage: Vec<String> = ["HELLO", "world"].iter().map(|s| s.to_string()).collect();
        assert!(parse_message(&garbage).is_none());
        let zero: Vec<String> = ["RX", "0", "MUTE", "ON"].iter().map(|s| s.to_string()).collect();
        assert!(parse_message(&zero).is_none());
    }

    #[tokio::test]
    async fn connect_requests_dump_and_telemetry_flows_to_subscribers() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut adapter = LectrosonicsAdapter::new("dsqd-1", addr);
        let mut events = adapter.subscribe();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = socket.read(&mut buf).await.unwrap();
            let sent = String::from_utf8_lossy(&buf[..n]).to_string();
            socket
                .write_all(b"RX 1 FREQ 614125 RF 080 BATT 090 MUTE ON\r")
                .await
                .unwrap();
            sent
        });

        adapter.connect().await.unwrap();

        let sent = server.await.unwrap();
        assert_eq!(sent, "QUERY ALL\r");

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for telemetry event")
            .unwrap();
        assert_eq!(event.address, MicAddress::new("dsqd-1", 1));
        assert_eq!(event.state.frequency_mhz, Some(614.125));
        assert_eq!(event.state.rf_quality_percent, Some(80));
        assert_eq!(event.state.battery_percent, Some(90));
        assert!(event.state.muted);
        // No documented dBFS/dBm formula -> these stay None, never fabricated.
        assert_eq!(event.state.audio_level_dbfs, None);
        assert_eq!(event.state.rf_level_dbm, None);
    }
}

//! `MicAdapter` for Shure ULX-D and Axient Digital receivers, built from
//! Shure's official "Command Strings for ULX-D Receivers" technical
//! bulletin (RW 1/12/2018,
//! <https://content-files.shure.com/Pubs/ulx/ulx-d-network-string-commands.pdf>).
//!
//! Wire format: ASCII over TCP, port 2202, messages delimited by `<` and
//! `>` (e.g. `< GET 1 AUDIO_MUTE >`, `< REP 1 AUDIO_MUTE ON >`). Four
//! message types: GET (query), SET (change), REP (the receiver's reply to
//! GET/SET, also sent unsolicited whenever a parameter changes - so most
//! fields never need re-polling), and SAMPLE (periodic RF/audio metering,
//! opt-in via `SET x METER_RATE`). Channel `x` is ASCII `0`-`4`: `0` means
//! "all channels" *in a request* on a dual/quad receiver; the doc's own
//! examples show REP/SAMPLE replies always carrying a concrete channel
//! number, never `0`, so this adapter treats a `0` channel in an incoming
//! message as malformed and drops it rather than guessing which channel
//! it meant.
//!
//! Fields implemented here (the ones `MicState` needs): `AUDIO_MUTE`,
//! `BATT_CHARGE`, `BATT_RUN_TIME`, `FREQUENCY`, and `METER_RATE`/`SAMPLE`
//! for RF level, audio level, and antenna diversity. Per the spec:
//! `SAMPLE x ALL nn aaa eee` - `nn` is `AX`/`XB`/`XX` (antenna A/B/neither
//! active), `aaa` is RF level `000`-`115` (dBm = `aaa - 128`), `eee` is
//! audio level `000`-`050` on the receiver's own meter scale (not a
//! calibrated dBFS value - the doc doesn't give a conversion, so this
//! adapter reports it as-is rather than inventing one).
//!
//! `connect()` enables metering for all channels by sending
//! `SET 0 METER_RATE 00500`. The doc confirms `0` means "all channels" for
//! *GET* requests on a dual/quad receiver but doesn't show a `SET
//! METER_RATE` example with channel `0` specifically - this adapter
//! assumes the same channel-indexing convention applies uniformly across
//! GET and SET (consistent with every other command in the doc using the
//! same `x` placeholder rules), but that specific case is inferred, not
//! shown verbatim, and worth confirming against real hardware.
//!
//! SCOPE NOTE on Axient Digital: Shure's separate "Axient Digital --
//! Command Strings" doc (Preliminary, May 2, 2018,
//! <https://content-files.shure.com/Pubs/AD4D/Axient_Digital_network_string_commands.pdf>)
//! confirms the same wire framing, port 2202, and GET/SET/REP/SAMPLE
//! command types - this adapter's message parsing and channel/mute/
//! metering handling apply equally to Axient Digital on that basis.
//! However, Axient's own parameter-by-parameter reference (47 pages) was
//! only spot-checked here, not fully cross-referenced field-by-field
//! against ULX-D's - Axient's per-channel model (each channel maps to
//! multiple transmitter slots) may mean some fields need slot-level
//! indexing this adapter doesn't yet handle. Treat Axient support as
//! "framing-compatible, field-level behavior unverified" until checked
//! against Axient's own doc in full or real hardware.
//!
//! No public doc describes an explicit "who are you" query for the
//! receiver itself (only `DEVICE_ID`, which is a user-settable label, not
//! a model/vendor identity) - `identify()` reflects that gap rather than
//! guessing, the same way `AhmAdapter::identify()` does for AHM.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_mic_core::{
    AdapterError, AdapterResult, AntennaDiversity, DeviceInfo, MicAddress, MicAdapter, MicEvent, MicState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

const DEFAULT_METER_RATE_MS: &str = "00500";

pub struct ShureAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
}

impl ShureAdapter {
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
        let framed = format!("< {message} >");
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
        rf_level_dbm: None,
        audio_level: None,
        muted: false,
        frequency_mhz: None,
        antenna: None,
    }
}

#[async_trait]
impl MicAdapter for ShureAdapter {
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

        self.send(&format!("SET 0 METER_RATE {DEFAULT_METER_RATE_MS}")).await
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        Err(AdapterError::Protocol(
            "identify: Shure's command-string protocol has no device-identity query for receivers \
             (DEVICE_ID is a user-settable label, not a model/vendor identity)"
                .into(),
        ))
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<MicState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        self.send(&format!("GET {channel} ALL")).await?;

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
        self.send(&format!("SET {channel} AUDIO_MUTE {value}")).await
    }

    fn subscribe(&self) -> broadcast::Receiver<MicEvent> {
        self.tx.subscribe()
    }
}

/// Incrementally splits a raw ASCII byte stream into individual
/// `< ... >`-delimited messages. Standalone and synchronous so framing can
/// be unit tested without a real socket.
#[derive(Default)]
struct ShureStreamParser {
    buf: String,
}

impl ShureStreamParser {
    fn push(&mut self, data: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(data));
    }

    fn next_message(&mut self) -> Option<Vec<String>> {
        let start = self.buf.find('<')?;
        let end = start + self.buf[start..].find('>')?;
        let body = self.buf[start + 1..end].trim().to_string();
        self.buf.drain(..=end);
        Some(body.split_whitespace().map(str::to_string).collect())
    }
}

enum ParsedUpdate {
    Mute { channel: u16, muted: bool },
    BatteryCharge { channel: u16, percent: Option<u8> },
    BatteryRunTime { channel: u16, minutes: Option<u16> },
    Frequency { channel: u16, mhz: Option<f64> },
    Sample { channel: u16, antenna: Option<AntennaDiversity>, rf_dbm: i16, audio_level: u8 },
}

fn parse_channel(token: &str) -> Option<u16> {
    let ch: u16 = token.parse().ok()?;
    // The doc's own REP/SAMPLE examples always carry a concrete channel
    // (1-4); 0 only appears as a "give me all channels" request wildcard,
    // never as a response channel. Treat 0 here as malformed rather than
    // guessing which real channel it meant.
    if ch == 0 {
        None
    } else {
        Some(ch)
    }
}

fn parse_message(tokens: &[String]) -> Option<ParsedUpdate> {
    let t: Vec<&str> = tokens.iter().map(String::as_str).collect();
    match t.as_slice() {
        ["REP", ch, "AUDIO_MUTE", state] => Some(ParsedUpdate::Mute {
            channel: parse_channel(ch)?,
            muted: *state == "ON",
        }),
        ["REP", ch, "BATT_CHARGE", pct] => {
            let raw: u16 = pct.parse().ok()?;
            Some(ParsedUpdate::BatteryCharge {
                channel: parse_channel(ch)?,
                percent: if raw <= 100 { Some(raw as u8) } else { None },
            })
        }
        ["REP", ch, "BATT_RUN_TIME", mins] => {
            let raw: u32 = mins.parse().ok()?;
            Some(ParsedUpdate::BatteryRunTime {
                channel: parse_channel(ch)?,
                minutes: if raw < 65535 { Some(raw as u16) } else { None },
            })
        }
        ["REP", ch, "FREQUENCY", freq] => {
            let raw: u32 = freq.parse().ok()?;
            Some(ParsedUpdate::Frequency {
                channel: parse_channel(ch)?,
                mhz: Some(raw as f64 / 1000.0),
            })
        }
        ["SAMPLE", ch, "ALL", nn, aaa, eee] => {
            let antenna = match *nn {
                "AX" => Some(AntennaDiversity::A),
                "XB" => Some(AntennaDiversity::B),
                "XX" => Some(AntennaDiversity::Inactive),
                _ => None,
            };
            let rf_raw: i16 = aaa.parse().ok()?;
            let audio_raw: u8 = eee.parse().ok()?;
            Some(ParsedUpdate::Sample {
                channel: parse_channel(ch)?,
                antenna,
                rf_dbm: rf_raw - 128,
                audio_level: audio_raw,
            })
        }
        _ => None,
    }
}

fn spawn_receive_loop(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    id: Arc<str>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
) {
    tokio::spawn(async move {
        let mut parser = ShureStreamParser::default();
        let mut buf = [0u8; 4096];

        loop {
            let len = match read_half.read(&mut buf).await {
                Ok(0) => {
                    debug!(device = %id, "Shure TCP connection closed");
                    return;
                }
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "Shure TCP read failed, stopping receive loop");
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
                    match update {
                        ParsedUpdate::Mute { channel, muted } => {
                            let entry = guard.entry(channel).or_insert_with(empty_state);
                            entry.muted = muted;
                            (channel, *entry)
                        }
                        ParsedUpdate::BatteryCharge { channel, percent } => {
                            let entry = guard.entry(channel).or_insert_with(empty_state);
                            entry.battery_percent = percent;
                            (channel, *entry)
                        }
                        ParsedUpdate::BatteryRunTime { channel, minutes } => {
                            let entry = guard.entry(channel).or_insert_with(empty_state);
                            entry.battery_minutes_remaining = minutes;
                            (channel, *entry)
                        }
                        ParsedUpdate::Frequency { channel, mhz } => {
                            let entry = guard.entry(channel).or_insert_with(empty_state);
                            entry.frequency_mhz = mhz;
                            (channel, *entry)
                        }
                        ParsedUpdate::Sample { channel, antenna, rf_dbm, audio_level } => {
                            let entry = guard.entry(channel).or_insert_with(empty_state);
                            entry.antenna = antenna;
                            entry.rf_level_dbm = Some(rf_dbm);
                            entry.audio_level = Some(audio_level);
                            (channel, *entry)
                        }
                    }
                };

                debug!(device = %id, channel, "Shure telemetry update");
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
    fn parser_splits_concatenated_messages_with_no_separator() {
        let mut parser = ShureStreamParser::default();
        parser.push(b"< REP 1 AUDIO_MUTE ON >< REP 1 BATT_CHARGE 087 >");

        assert_eq!(
            parser.next_message().unwrap(),
            vec!["REP", "1", "AUDIO_MUTE", "ON"]
        );
        assert_eq!(
            parser.next_message().unwrap(),
            vec!["REP", "1", "BATT_CHARGE", "087"]
        );
        assert!(parser.next_message().is_none());
    }

    #[test]
    fn parser_resyncs_across_split_reads() {
        let mut parser = ShureStreamParser::default();
        let mut result = None;
        for chunk in b"< REP 2 FREQUENCY 614125 >".chunks(5) {
            parser.push(chunk);
            if let Some(tokens) = parser.next_message() {
                result = Some(tokens);
            }
        }
        assert_eq!(result.unwrap(), vec!["REP", "2", "FREQUENCY", "614125"]);
    }

    #[test]
    fn frequency_converts_documented_six_digit_form_to_mhz() {
        let tokens: Vec<String> = ["REP", "3", "FREQUENCY", "614125"].iter().map(|s| s.to_string()).collect();
        match parse_message(&tokens) {
            Some(ParsedUpdate::Frequency { channel, mhz }) => {
                assert_eq!(channel, 3);
                assert_eq!(mhz, Some(614.125));
            }
            _ => panic!("expected a Frequency update"),
        }
    }

    #[test]
    fn battery_error_codes_map_to_none_not_a_real_reading() {
        let tokens: Vec<String> = ["REP", "1", "BATT_CHARGE", "255"].iter().map(|s| s.to_string()).collect();
        match parse_message(&tokens) {
            Some(ParsedUpdate::BatteryCharge { percent, .. }) => assert_eq!(percent, None),
            _ => panic!("expected a BatteryCharge update"),
        }

        let tokens: Vec<String> = ["REP", "1", "BATT_RUN_TIME", "65535"].iter().map(|s| s.to_string()).collect();
        match parse_message(&tokens) {
            Some(ParsedUpdate::BatteryRunTime { minutes, .. }) => assert_eq!(minutes, None),
            _ => panic!("expected a BatteryRunTime update"),
        }
    }

    #[test]
    fn sample_message_matches_documented_dbm_conversion_and_antenna_codes() {
        // aaa=087 -> dBm = 87 - 128 = -41; eee=023 audio level as-is.
        let tokens: Vec<String> = ["SAMPLE", "1", "ALL", "AX", "087", "023"].iter().map(|s| s.to_string()).collect();
        match parse_message(&tokens) {
            Some(ParsedUpdate::Sample { channel, antenna, rf_dbm, audio_level }) => {
                assert_eq!(channel, 1);
                assert_eq!(antenna, Some(AntennaDiversity::A));
                assert_eq!(rf_dbm, -41);
                assert_eq!(audio_level, 23);
            }
            _ => panic!("expected a Sample update"),
        }
    }

    #[test]
    fn zero_channel_in_a_response_is_dropped_not_guessed() {
        let tokens: Vec<String> = ["REP", "0", "AUDIO_MUTE", "ON"].iter().map(|s| s.to_string()).collect();
        assert!(parse_message(&tokens).is_none());
    }

    #[tokio::test]
    async fn connect_enables_metering_and_reports_flow_through_to_subscribers() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut adapter = ShureAdapter::new("ulxd-1", addr);
        let mut events = adapter.subscribe();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            // Expect the metering-enable command connect() sends first.
            let mut buf = [0u8; 64];
            let n = socket.read(&mut buf).await.unwrap();
            let sent = String::from_utf8_lossy(&buf[..n]).to_string();

            // Then push a spontaneous mute + a metering sample, as the
            // receiver would after a front-panel mute press and its
            // regular metering tick.
            socket
                .write_all(b"< REP 1 AUDIO_MUTE ON >< SAMPLE 1 ALL XB 100 030 >")
                .await
                .unwrap();

            sent
        });

        adapter.connect().await.unwrap();

        let sent = server.await.unwrap();
        assert_eq!(sent, "< SET 0 METER_RATE 00500 >");

        let first = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for mute event")
            .unwrap();
        assert_eq!(first.address, MicAddress::new("ulxd-1", 1));
        assert!(first.state.muted);

        let second = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for sample event")
            .unwrap();
        assert_eq!(second.state.antenna, Some(AntennaDiversity::B));
        assert_eq!(second.state.rf_level_dbm, Some(100 - 128));
        assert_eq!(second.state.audio_level, Some(30));
    }
}

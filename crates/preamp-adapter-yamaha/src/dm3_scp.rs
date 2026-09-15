//! `DeviceAdapter` for Yamaha DM3/DM3S over **SCP** - the console's
//! newline-delimited ASCII "Remote Control Protocol" on **TCP 49280**.
//!
//! This is the DM3's *primary* control surface and the one path in this
//! project **validated end-to-end against real hardware** (a DM3, firmware
//! V3.00, on 2026-09-15). See
//! [`docs/yamaha-dm3-bench-2026-09-15.md`](../../../docs/yamaha-dm3-bench-2026-09-15.md)
//! for the capture and the full protocol write-up; every request/reply
//! shape below was observed on that console, not inferred from a document.
//!
//! Why SCP and not the OSC port [`crate::Dm3Adapter`] uses: over SCP a
//! `get`/`set` is **confirmed** by an `OK ...` reply that echoes the value,
//! and the console **pushes `NOTIFY set ...`** for every change - made by a
//! surface, by us, or by any other controller - to every connected client
//! with no subscription step. That makes both `get_state` (no stale cache)
//! and `subscribe` (a real change feed) work correctly, neither of which
//! the OSC transport can do: OSC `set` is unacknowledged and the DM3 emits
//! no unsolicited OSC. See the bench doc's "What this changes for the
//! bridge" section.
//!
//! Protocol, as observed:
//!   - Transport: plain TCP, one command per line, `\n`-terminated ASCII,
//!     replies likewise. Port 49280.
//!   - Head-amp gain: `IO:Current/InCh/HAGain <x> <y> <v>` where `<x>` is a
//!     **0-based** Local Input index (0..15 on a DM3), `<y>` is always `0`,
//!     and `<v>` is gain in **whole dB, 0..64** (direct, not scaled).
//!   - Phantom: `IO:Current/InCh/48VOn <x> 0 <0|1>`.
//!   - Read: `get <addr> <x> 0` -> `OK get <addr> <x> 0 <v>`.
//!   - Write: `set <addr> <x> 0 <v>` -> `OK set <addr> <x> 0 <v> "<disp>"`,
//!     plus an unsolicited `NOTIFY set <addr> <x> 0 <v> "<disp>"` broadcast.
//!   - Identify: `devinfo productname` -> `OK devinfo productname "DM3"`.
//!   - Errors: `ERROR <command> <Reason>` (e.g. `UnknownAddress`,
//!     `InvalidArgument`). The session expects a periodic heartbeat; this
//!     adapter sends `devstatus runmode` every 20 s (proven to hold the
//!     session open) after setting `scpmode keepalive`.
//!
//! Channel numbering at this adapter's API is **1-based** (channel 1 =
//! Local Input 1 = SCP index 0), matching every other adapter here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    ChangedFields, AdapterError, AdapterResult, DeviceAdapter, DeviceInfo, PreampAddress,
    PreampEvent, PreampState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// The DM3's documented SCP port.
pub const SCP_PORT: u16 = 49280;

const HA_GAIN_ADDR: &str = "IO:Current/InCh/HAGain";
const PHANTOM_ADDR: &str = "IO:Current/InCh/48VOn";
const GAIN_MIN_DB: f32 = 0.0;
const GAIN_MAX_DB: f32 = 64.0;
const HEARTBEAT: Duration = Duration::from_secs(20);

type PendingIdentify = Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>;

pub struct Dm3ScpAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    writer: Option<Arc<Mutex<OwnedWriteHalf>>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: PendingIdentify,
    cancel: CancellationToken,
}

impl Dm3ScpAdapter {
    pub fn new(id: impl Into<Arc<str>>, remote: SocketAddr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote,
            writer: None,
            tx,
            state: Arc::new(Mutex::new(HashMap::new())),
            pending_identify: Arc::new(StdMutex::new(None)),
            cancel: CancellationToken::new(),
        }
    }

    /// DM3/DM3S support Local Input 1-16. Reject obviously-invalid input
    /// rather than enforcing a hard per-model ceiling (other DM-family
    /// models are unconfirmed), mirroring [`crate::Dm3Adapter`].
    fn scp_index(channel: u16) -> AdapterResult<u16> {
        if channel == 0 || channel > 128 {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(channel - 1)
    }

    async fn send_line(&self, line: String) -> AdapterResult<()> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        writer
            .lock()
            .await
            .write_all(&bytes)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }
}

#[async_trait]
impl DeviceAdapter for Dm3ScpAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let stream = TcpStream::connect(self.remote)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        let (read_half, write_half) = stream.into_split();
        let writer = Arc::new(Mutex::new(write_half));
        self.writer = Some(Arc::clone(&writer));

        spawn_receive_loop(
            read_half,
            Arc::clone(&self.id),
            self.tx.clone(),
            Arc::clone(&self.state),
            Arc::clone(&self.pending_identify),
            self.remote.ip(),
            self.cancel.clone(),
        );
        spawn_heartbeat(Arc::clone(&writer), Arc::clone(&self.id), self.cancel.clone());

        // Ask the device to hold the session open; our heartbeat stays well
        // inside this. Best-effort: if the write fails, connect() has
        // already succeeded and the receive loop will surface a dead link.
        let _ = self.send_line("scpmode keepalive 60000".to_string()).await;
        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        self.writer = None;
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        let (tx, rx) = oneshot::channel();
        *self.pending_identify.lock().unwrap() = Some(tx);
        self.send_line("devinfo productname".to_string()).await?;
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(|_| AdapterError::Protocol("identify: timed out waiting for devinfo reply".into()))?
            .map_err(|_| AdapterError::Protocol("identify: reply channel dropped".into()))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let x = Self::scp_index(channel)?;
        let v = gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i32;
        self.send_line(format!("set {HA_GAIN_ADDR} {x} 0 {v}")).await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let x = Self::scp_index(channel)?;
        self.send_line(format!("set {PHANTOM_ADDR} {x} 0 {}", on as u8)).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        let x = Self::scp_index(channel)?;
        // Always issue the reads. The cache is kept live by NOTIFY, but a
        // fresh get confirms the current value and populates it on the
        // first call. Replies land on the receive loop, which fills the
        // cache; poll for it.
        self.send_line(format!("get {HA_GAIN_ADDR} {x} 0")).await?;
        self.send_line(format!("get {PHANTOM_ADDR} {x} 0")).await?;

        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!(
            "no reply for channel {channel} preamp state over SCP"
        )))
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

fn spawn_heartbeat(writer: Arc<Mutex<OwnedWriteHalf>>, id: Arc<str>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEARTBEAT);
        ticker.tick().await; // fire the first tick immediately, skip it
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = ticker.tick() => {
                    if writer.lock().await.write_all(b"devstatus runmode\n").await.is_err() {
                        debug!(device = %id, "SCP heartbeat write failed; link is down");
                        return;
                    }
                }
            }
        }
    });
}

fn spawn_receive_loop(
    mut read_half: OwnedReadHalf,
    id: Arc<str>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: PendingIdentify,
    remote_ip: std::net::IpAddr,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let mut line = Vec::<u8>::new();
        loop {
            let n = tokio::select! {
                _ = cancel.cancelled() => return,
                result = read_half.read(&mut buf) => match result {
                    Ok(0) => {
                        warn!(device = %id, "DM3 SCP connection closed by peer");
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        warn!(device = %id, error = %e, "DM3 SCP socket read failed, stopping receive loop");
                        return;
                    }
                },
            };
            for &b in &buf[..n] {
                if b == b'\n' {
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    line.clear();
                    if !text.is_empty() {
                        handle_line(&text, &id, &tx, &state, &pending_identify, remote_ip).await;
                    }
                } else if b != b'\r' {
                    line.push(b);
                    if line.len() > 8192 {
                        // Runaway line with no terminator - drop it rather
                        // than grow unbounded.
                        line.clear();
                    }
                }
            }
        }
    });
}

/// One SCP reply/notification line. Recognises the two head-amp addresses
/// in `OK get`, `OK set` and `NOTIFY set` lines (all three carry the value),
/// and the `devinfo productname` identify reply. Everything else -
/// `OK devstatus`, `OK scpmode`, `ERROR ...`, other addresses - is ignored.
async fn handle_line(
    text: &str,
    id: &Arc<str>,
    tx: &broadcast::Sender<PreampEvent>,
    state: &Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: &PendingIdentify,
    remote_ip: std::net::IpAddr,
) {
    let tokens: Vec<&str> = text.split_whitespace().collect();

    // Identify: OK devinfo productname "DM3"
    if let Some(model) = parse_devinfo_productname(&tokens) {
        if let Some(sender) = pending_identify.lock().unwrap().take() {
            let _ = sender.send(DeviceInfo {
                vendor: "Yamaha".to_string(),
                model,
                address: remote_ip,
            });
        }
        return;
    }

    let Some((channel, field, value)) = parse_headamp_line(&tokens) else {
        return;
    };

    let (new_state, changed) = {
        let mut guard = state.lock().await;
        let entry = guard.entry(channel).or_insert(PreampState {
            gain_db: 0.0,
            phantom: false,
            pad: None,
        });
        let changed = match field {
            HeadampField::Gain => {
                entry.gain_db = value as f32;
                ChangedFields::GAIN
            }
            HeadampField::Phantom => {
                entry.phantom = value != 0;
                ChangedFields::PHANTOM
            }
        };
        (*entry, changed)
    };

    debug!(device = %id, channel, ?field, value, "DM3 SCP headamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
        changed,
    });
}

#[derive(Debug, Clone, Copy)]
enum HeadampField {
    Gain,
    Phantom,
}

/// `OK devinfo productname "DM3"` -> `Some("DM3")`. Tolerant of the reply
/// being `OK`- or (hypothetically) `NOTIFY`-prefixed.
fn parse_devinfo_productname(tokens: &[&str]) -> Option<String> {
    // find "devinfo" "productname" adjacent, take the quoted remainder
    let pos = tokens.iter().position(|&t| t == "devinfo")?;
    if tokens.get(pos + 1)? != &"productname" {
        return None;
    }
    let raw = tokens.get(pos + 2)?;
    Some(raw.trim_matches('"').to_string())
}

/// Parse a head-amp value out of an `OK get`, `OK set` or `NOTIFY set` line.
/// Layout: `<...> <addr> <x> <y> <v> [<"disp">]`. Returns the **1-based**
/// channel, which field, and the integer value.
fn parse_headamp_line(tokens: &[&str]) -> Option<(u16, HeadampField, i32)> {
    let idx = tokens
        .iter()
        .position(|&t| t == HA_GAIN_ADDR || t == PHANTOM_ADDR)?;
    let field = if tokens[idx] == HA_GAIN_ADDR {
        HeadampField::Gain
    } else {
        HeadampField::Phantom
    };
    let x: u16 = tokens.get(idx + 1)?.parse().ok()?;
    // tokens[idx+2] is the y index (always 0); the value is next.
    let value: i32 = tokens.get(idx + 3)?.parse().ok()?;
    Some((x.checked_add(1)?, field, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn scp_index_is_zero_based_and_bounded() {
        assert_eq!(Dm3ScpAdapter::scp_index(1).unwrap(), 0);
        assert_eq!(Dm3ScpAdapter::scp_index(16).unwrap(), 15);
        assert!(Dm3ScpAdapter::scp_index(0).is_err());
        assert!(Dm3ScpAdapter::scp_index(200).is_err());
    }

    #[test]
    fn parses_a_real_get_reply() {
        // captured: "OK get IO:Current/InCh/HAGain 8 0 23"
        let t: Vec<&str> = "OK get IO:Current/InCh/HAGain 8 0 23".split_whitespace().collect();
        assert!(matches!(parse_headamp_line(&t), Some((9, HeadampField::Gain, 23))));
    }

    #[test]
    fn parses_a_real_notify_set_line() {
        // captured: NOTIFY set IO:Current/InCh/48VOn 8 0 1 "ON"
        let t: Vec<&str> = "NOTIFY set IO:Current/InCh/48VOn 8 0 1 \"ON\"".split_whitespace().collect();
        assert!(matches!(parse_headamp_line(&t), Some((9, HeadampField::Phantom, 1))));
    }

    #[test]
    fn parses_a_real_set_reply_with_display_string() {
        // captured: OK set IO:Current/InCh/HAGain 8 0 12 "+12"
        let t: Vec<&str> = "OK set IO:Current/InCh/HAGain 8 0 12 \"+12\"".split_whitespace().collect();
        assert!(matches!(parse_headamp_line(&t), Some((9, HeadampField::Gain, 12))));
    }

    #[test]
    fn ignores_unrelated_lines() {
        for line in [
            "OK devstatus runmode \"normal\"",
            "OK scpmode keepalive 60000",
            "ERROR get UnknownAddress",
            "OK get MIXER:Current/InCh/Fader/Level 0 0 -240",
        ] {
            let t: Vec<&str> = line.split_whitespace().collect();
            assert!(parse_headamp_line(&t).is_none(), "should ignore: {line}");
        }
    }

    #[test]
    fn parses_the_devinfo_identify_reply() {
        let t: Vec<&str> = "OK devinfo productname \"DM3\"".split_whitespace().collect();
        assert_eq!(parse_devinfo_productname(&t).as_deref(), Some("DM3"));
        let t2: Vec<&str> = "OK devstatus runmode \"normal\"".split_whitespace().collect();
        assert_eq!(parse_devinfo_productname(&t2), None);
    }

    #[tokio::test]
    async fn set_gain_writes_the_documented_scp_line() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            // first line is the scpmode keepalive on connect; read until we
            // see the set line.
            let mut acc = String::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.contains("set ") {
                    break;
                }
            }
            acc
        });

        let mut adapter = Dm3ScpAdapter::new("dm3", addr);
        adapter.connect().await.unwrap();
        adapter.set_gain(9, 12.0).await.unwrap();
        let seen = server.await.unwrap();
        assert!(
            seen.contains("set IO:Current/InCh/HAGain 8 0 12"),
            "wire did not carry the documented line, got: {seen:?}"
        );
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn notify_set_surfaces_through_subscribe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Push an unsolicited NOTIFY, exactly as a real DM3 does when
            // another controller moves a preamp.
            sock.write_all(b"NOTIFY set IO:Current/InCh/HAGain 4 0 30 \"+30\"\n")
                .await
                .unwrap();
            // keep the socket open briefly so the client can read it
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let mut adapter = Dm3ScpAdapter::new("dm3", addr);
        adapter.connect().await.unwrap();
        let mut rx = adapter.subscribe();
        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event within timeout")
            .expect("event");
        assert_eq!(ev.address.channel, 5);
        assert_eq!(ev.state.gain_db, 30.0);
        assert!(ev.changed.gain);
        server.await.unwrap();
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn identify_resolves_from_a_devinfo_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            // drain until the devinfo request arrives, then answer
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                if String::from_utf8_lossy(&buf[..n]).contains("devinfo productname") {
                    break;
                }
            }
            sock.write_all(b"OK devinfo productname \"DM3\"\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let mut adapter = Dm3ScpAdapter::new("dm3", addr);
        adapter.connect().await.unwrap();
        let info = adapter.identify().await.unwrap();
        assert_eq!(info.vendor, "Yamaha");
        assert_eq!(info.model, "DM3");
        server.await.unwrap();
        adapter.disconnect().await.unwrap();
    }
}

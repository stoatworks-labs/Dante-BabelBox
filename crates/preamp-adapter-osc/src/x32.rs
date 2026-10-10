//! `DeviceAdapter` for the X32-family OSC dialect: Behringer X32, Midas M32
//! and Midas HD96 all share this protocol (M32/HD96 are the same firmware
//! lineage as X32 under a different badge). Source: Patrick-Gilles Maillot's
//! "Unofficial X32/M32 OSC Remote Protocol", v4.06-09 (firmware 4.0+), the
//! community reference (a copy ships in Bitfocus Companion's X32 module as
//! `docs/X32-OSC.pdf`).
//!
//! Wire format: plain OSC over UDP, default port 10023. Sending an address
//! with no argument is a "get" - the console replies with the same address
//! and its current value. **Every binary float goes over the wire as
//! 0.0..=1.0** ("for binary floats, the data sent to or returned from the
//! X32/M32 is always in the range [0.0 ... 1.0]"); the document gives each
//! parameter's real range and step, e.g. `linf [-12.000, 60.000, 0.500]`,
//! which [`DbLaw`] converts to and from.
//!
//! Headamps: `/headamp/[000…127]/gain linf [-12.000, 60.000, 0.500]` and
//! `/headamp/[000…127]/phantom` (int 0/1); 000-031 are the console's local
//! inputs, 032-079 AES50-A, 080-127 AES50-B. This adapter's `channel` is the
//! headamp index plus one, so `channel` 1..=128.
//!
//! Up to 0.4.1 this adapter addressed `/headamp/01`…`/24` and sent the gain
//! as a raw dB float, which an X32 reads as a 0..1 value - anything over 1 dB
//! would have landed at full scale, +60 dB. It never met a console. Both were
//! corrected against the document when the gain lock started setting gains
//! through this adapter.
//!
//! Trim, for the gain lock's follower side: `/ch/[01…32]/preamp/trim linf
//! [-18.000, 18.000, 0.250]`, which the document marks "(digital sources
//! only)". An input channel fed by a local or AES50 headamp has headamp gain
//! there instead - unless that console's "HA gain split" mode
//! (`/-prefs/haflags`) is on for the port, which forum reports (not the
//! document) say gives such channels their own ±18 dB trim. Unverified
//! either way; check it on the desk before relying on it. The protocol has
//! no relative command at all, so [`DeviceAdapter::nudge_trim`] reads the
//! live trim and writes the new one straight after.
//!
//! The console only pushes value changes (someone turning a physical or
//! on-screen knob) to clients that keep sending `/xremote` at least every
//! ~10s, so `connect()` starts that heartbeat. Whether it also echoes a
//! change back to the client that made it isn't stated, so a write updates
//! this adapter's own cache rather than waiting for an echo.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    ChangedFields, AdapterError, AdapterResult, DbLaw, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent,
    PreampState,
};
use rosc::{OscMessage, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// `/headamp/[000…127]/gain linf [-12.000, 60.000, 0.500]`.
pub const HEADAMP_LAW: DbLaw = DbLaw::new(-12.0, 60.0, 0.5);
/// `/ch/[01…32]/preamp/trim linf [-18.000, 18.000, 0.250]`.
pub const TRIM_LAW: DbLaw = DbLaw::new(-18.0, 18.0, 0.25);
/// Headamps 000-127: 32 local, 48 on each AES50 port.
pub const HEADAMPS: u16 = 128;
/// Input channels 01-32.
pub const TRIM_INPUTS: u16 = 32;

/// How long a read waits for the console's reply.
const READ_ATTEMPTS: usize = 20;
const READ_POLL: Duration = Duration::from_millis(50);
/// How long the receive loop waits after a failed read before trying again.
const READ_ERROR_BACKOFF: Duration = Duration::from_millis(250);

type TrimCache = Arc<Mutex<HashMap<u16, f32>>>;

/// What the receive loop keeps current: preamp channels and input trims.
#[derive(Clone)]
struct Caches {
    preamps: Arc<Mutex<HashMap<u16, PreampState>>>,
    trims: TrimCache,
}

pub struct X32Adapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
    trims: TrimCache,
    pending_identify: Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>,
    cancel: CancellationToken,
}

impl X32Adapter {
    pub fn new(id: impl Into<Arc<str>>, remote: SocketAddr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote,
            socket: None,
            tx,
            state: Arc::new(Mutex::new(HashMap::new())),
            trims: Arc::new(Mutex::new(HashMap::new())),
            pending_identify: Arc::new(StdMutex::new(None)),
            cancel: CancellationToken::new(),
        }
    }

    fn headamp_path(channel: u16, suffix: &str) -> AdapterResult<String> {
        if !(1..=HEADAMPS).contains(&channel) {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(format!("/headamp/{:03}/{suffix}", channel - 1))
    }

    fn trim_path(input: u16) -> AdapterResult<String> {
        if !(1..=TRIM_INPUTS).contains(&input) {
            return Err(AdapterError::UnsupportedChannel(input));
        }
        Ok(format!("/ch/{input:02}/preamp/trim"))
    }

    async fn send(&self, addr: &str, args: Vec<OscType>) -> AdapterResult<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let packet = OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args,
        });
        let bytes = rosc::encoder::encode(&packet)
            .map_err(|e| AdapterError::Protocol(format!("OSC encode failed: {e:?}")))?;
        socket
            .send(&bytes)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl DeviceAdapter for X32Adapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        socket
            .connect(self.remote)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        let socket = Arc::new(socket);
        self.socket = Some(Arc::clone(&socket));

        spawn_receive_loop(
            Arc::clone(&socket),
            Arc::clone(&self.id),
            self.tx.clone(),
            Caches { preamps: Arc::clone(&self.state), trims: Arc::clone(&self.trims) },
            Arc::clone(&self.pending_identify),
            self.remote.ip(),
            self.cancel.clone(),
        );
        spawn_xremote_heartbeat(Arc::clone(&socket), self.cancel.clone());

        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        let (tx, rx) = oneshot::channel();
        *self.pending_identify.lock().unwrap() = Some(tx);
        self.send("/info", vec![]).await?;
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(|_| AdapterError::Protocol("identify: timed out waiting for /info reply".into()))?
            .map_err(|_| AdapterError::Protocol("identify: reply channel dropped".into()))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let addr = Self::headamp_path(channel, "gain")?;
        self.send(&addr, vec![OscType::Float(HEADAMP_LAW.to_unit(gain_db))]).await?;
        // Only refresh a channel already read: inserting one here would
        // make up a phantom value nobody read (see ChangedFields).
        if let Some(entry) = self.state.lock().await.get_mut(&channel) {
            entry.gain_db = HEADAMP_LAW.quantize(gain_db);
        }
        Ok(())
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let addr = Self::headamp_path(channel, "phantom")?;
        self.send(&addr, vec![OscType::Int(on as i32)]).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if let Some(state) = self.state.lock().await.get(&channel).filter(|s| s.gain_known()) {
            return Ok(*state);
        }
        self.send(&Self::headamp_path(channel, "gain")?, vec![]).await?;
        self.send(&Self::headamp_path(channel, "phantom")?, vec![]).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel).filter(|s| s.gain_known()) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!(
            "no reply for channel {channel} headamp state"
        )))
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }

    fn trim_inputs(&self) -> u16 {
        TRIM_INPUTS
    }

    /// Always asks the console - a cached trim could be stale if a push was
    /// missed, and a relative move made from a stale value would overwrite
    /// the change it missed.
    async fn get_trim(&mut self, input: u16) -> AdapterResult<f32> {
        let addr = Self::trim_path(input)?;
        self.trims.lock().await.remove(&input);
        self.send(&addr, vec![]).await?;
        for _ in 0..READ_ATTEMPTS {
            tokio::time::sleep(READ_POLL).await;
            if let Some(trim) = self.trims.lock().await.get(&input) {
                return Ok(*trim);
            }
        }
        Err(AdapterError::Protocol(format!("no reply for input {input} trim")))
    }

    async fn set_trim(&mut self, input: u16, trim_db: f32) -> AdapterResult<()> {
        let addr = Self::trim_path(input)?;
        self.send(&addr, vec![OscType::Float(TRIM_LAW.to_unit(trim_db))]).await?;
        self.trims.lock().await.insert(input, TRIM_LAW.quantize(trim_db));
        Ok(())
    }

    async fn nudge_trim(&mut self, input: u16, delta_db: f32) -> AdapterResult<f32> {
        Self::trim_path(input)?;
        if !TRIM_LAW.moves(delta_db) {
            return Ok(0.0);
        }
        let current = self.get_trim(input).await?;
        let (target, applied) = TRIM_LAW.nudge(current, delta_db);
        if applied != 0.0 {
            self.set_trim(input, target).await?;
        }
        Ok(applied)
    }
}

#[derive(Debug, Clone, Copy)]
enum HeadampField {
    Gain,
    Phantom,
}

/// `/headamp/032/gain` -> `(33, Gain)`: the wire index is 0-based, this
/// adapter's `channel` 1-based.
fn parse_headamp_addr(addr: &str) -> Option<(u16, HeadampField)> {
    let rest = addr.strip_prefix("/headamp/")?;
    let (chan_str, field_str) = rest.split_once('/')?;
    if chan_str.len() != 3 {
        return None;
    }
    let channel = chan_str.parse::<u16>().ok()?.checked_add(1).filter(|c| *c <= HEADAMPS)?;
    let field = match field_str {
        "gain" => HeadampField::Gain,
        "phantom" => HeadampField::Phantom,
        _ => return None,
    };
    Some((channel, field))
}

/// `/ch/07/preamp/trim` -> `7`.
fn parse_trim_addr(addr: &str) -> Option<u16> {
    let rest = addr.strip_prefix("/ch/")?.strip_suffix("/preamp/trim")?;
    if rest.len() != 2 {
        return None;
    }
    rest.parse::<u16>().ok().filter(|i| (1..=TRIM_INPUTS).contains(i))
}

type PendingIdentify = Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>;

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    tx: broadcast::Sender<PreampEvent>,
    caches: Caches,
    pending_identify: PendingIdentify,
    remote_ip: IpAddr,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let len = tokio::select! {
                _ = cancel.cancelled() => return,
                result = socket.recv(&mut buf) => match result {
                    Ok(len) => len,
                    Err(e) => {
                        // A connected UDP socket reports an ICMP "port
                        // unreachable" as an error on the next read - which
                        // is what a console rebooting looks like. That used
                        // to end this loop, leaving the adapter deaf to the
                        // desk for good once it came back. Ride it out.
                        debug!(device = %id, error = %e, "X32 OSC socket read failed; retrying");
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = tokio::time::sleep(READ_ERROR_BACKOFF) => continue,
                        }
                    }
                },
            };
            match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, packet)) => {
                    handle_packet(packet, &id, &tx, &caches, &pending_identify, remote_ip).await
                }
                Err(e) => debug!(device = %id, error = ?e, "dropping malformed OSC packet"),
            }
        }
    });
}

fn handle_packet<'a>(
    packet: OscPacket,
    id: &'a Arc<str>,
    tx: &'a broadcast::Sender<PreampEvent>,
    caches: &'a Caches,
    pending_identify: &'a PendingIdentify,
    remote_ip: IpAddr,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        match packet {
            OscPacket::Message(msg) => {
                handle_message(msg, id, tx, caches, pending_identify, remote_ip).await
            }
            OscPacket::Bundle(bundle) => {
                for p in bundle.content {
                    handle_packet(p, id, tx, caches, pending_identify, remote_ip).await;
                }
            }
        }
    })
}

/// Parses the `/info` reply: 4 strings `[osc-version, "osc-server", model,
/// firmware-version]` (confirmed against x32ram.com's published protocol
/// doc and real-world examples for X32/X32RACK/X32C/X32P/X32CORE and
/// M32/M32C/M32R). Vendor is inferred from the model prefix since the
/// reply carries no separate vendor field.
fn parse_info_reply(msg: &OscMessage, address: IpAddr) -> Option<DeviceInfo> {
    if msg.addr != "/info" {
        return None;
    }
    let model = match msg.args.get(2) {
        Some(OscType::String(s)) => s.clone(),
        _ => return None,
    };
    let vendor = if model.starts_with('M') { "Midas" } else { "Behringer" };
    Some(DeviceInfo {
        vendor: vendor.to_string(),
        model,
        address,
    })
}

async fn handle_message(
    msg: OscMessage,
    id: &Arc<str>,
    tx: &broadcast::Sender<PreampEvent>,
    caches: &Caches,
    pending_identify: &PendingIdentify,
    remote_ip: IpAddr,
) {
    if let Some(info) = parse_info_reply(&msg, remote_ip) {
        if let Some(sender) = pending_identify.lock().unwrap().take() {
            let _ = sender.send(info);
        }
        return;
    }

    if let Some(input) = parse_trim_addr(&msg.addr) {
        if let Some(OscType::Float(unit)) = msg.args.first() {
            let trim_db = TRIM_LAW.from_unit(*unit);
            debug!(device = %id, input, trim_db, "X32 trim update");
            caches.trims.lock().await.insert(input, trim_db);
        }
        return;
    }

    let Some((channel, field)) = parse_headamp_addr(&msg.addr) else {
        return;
    };
    let value = match (field, msg.args.first()) {
        (HeadampField::Gain, Some(OscType::Float(unit))) => HEADAMP_LAW.from_unit(*unit),
        (HeadampField::Phantom, Some(OscType::Int(i))) => *i as f32,
        _ => return,
    };

    let (new_state, changed) = {
        let mut guard = caches.preamps.lock().await;
        let entry = guard.entry(channel).or_insert(PreampState::UNREAD);
        // One field per message; see ChangedFields.
        let changed = match field {
            HeadampField::Gain => {
                entry.gain_db = value;
                ChangedFields::GAIN
            }
            HeadampField::Phantom => {
                entry.phantom = value != 0.0;
                ChangedFields::PHANTOM
            }
        };
        (*entry, changed)
    };

    debug!(device = %id, channel, ?field, value, "X32 headamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
        changed,
    });
}

fn spawn_xremote_heartbeat(socket: Arc<UdpSocket>, cancel: CancellationToken) {
    tokio::spawn(async move {
        loop {
            let packet = OscPacket::Message(OscMessage {
                addr: "/xremote".to_string(),
                args: vec![],
            });
            match rosc::encoder::encode(&packet) {
                Ok(bytes) => {
                    // Keep beating through an outage: the console only
                    // pushes changes to clients that renew, so stopping here
                    // would leave the adapter deaf once the desk is back.
                    if let Err(e) = socket.send(&bytes).await {
                        debug!(error = %e, "failed to send /xremote heartbeat; will retry");
                    }
                }
                Err(e) => warn!(error = ?e, "failed to encode /xremote heartbeat"),
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(9)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headamp_path_is_the_documents_zero_based_three_digit_index() {
        assert_eq!(X32Adapter::headamp_path(1, "gain").unwrap(), "/headamp/000/gain");
        assert_eq!(X32Adapter::headamp_path(33, "gain").unwrap(), "/headamp/032/gain"); // AES50-A 1
        assert_eq!(X32Adapter::headamp_path(128, "phantom").unwrap(), "/headamp/127/phantom");
        assert!(X32Adapter::headamp_path(0, "gain").is_err());
        assert!(X32Adapter::headamp_path(129, "gain").is_err());
    }

    #[test]
    fn parse_headamp_addr_round_trips_known_paths() {
        assert!(matches!(parse_headamp_addr("/headamp/000/gain"), Some((1, HeadampField::Gain))));
        assert!(matches!(parse_headamp_addr("/headamp/127/phantom"), Some((128, HeadampField::Phantom))));
        assert!(parse_headamp_addr("/ch/01/mix/fader").is_none());
        assert!(parse_headamp_addr("/headamp/000/pad").is_none());
        assert!(parse_headamp_addr("/headamp/01/gain").is_none(), "two-digit indices are not X32's");
        assert!(parse_headamp_addr("/headamp/128/gain").is_none());
    }

    #[test]
    fn trim_paths_round_trip() {
        assert_eq!(X32Adapter::trim_path(7).unwrap(), "/ch/07/preamp/trim");
        assert_eq!(parse_trim_addr("/ch/07/preamp/trim"), Some(7));
        assert_eq!(parse_trim_addr("/ch/32/preamp/trim"), Some(32));
        assert!(X32Adapter::trim_path(33).is_err());
        assert_eq!(parse_trim_addr("/ch/33/preamp/trim"), None);
        assert_eq!(parse_trim_addr("/ch/07/preamp/hpf"), None);
    }

    /// A raw dB float is what this adapter used to send; X32 reads floats as
    /// 0..1, so +30 dB would have been clamped to 1.0 = +60 dB.
    #[tokio::test]
    async fn set_gain_sends_the_normalised_float_not_decibels() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut adapter = X32Adapter::new("x32-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();

        adapter.set_gain(33, 30.0).await.unwrap();
        let m = recv_ignoring_heartbeat(&mock).await.0;
        assert_eq!(m.addr, "/headamp/032/gain");
        assert_eq!(m.args, vec![OscType::Float((30.0 + 12.0) / 72.0)]);
    }

    /// Receives the next message that isn't the `/xremote` heartbeat.
    async fn recv_ignoring_heartbeat(mock: &UdpSocket) -> (OscMessage, SocketAddr) {
        let mut buf = [0u8; 512];
        loop {
            let (len, from) = tokio::time::timeout(Duration::from_secs(2), mock.recv_from(&mut buf))
                .await
                .expect("timed out waiting for the adapter")
                .unwrap();
            if let (_, OscPacket::Message(m)) = rosc::decoder::decode_udp(&buf[..len]).unwrap() {
                if m.addr != "/xremote" {
                    return (m, from);
                }
            }
        }
    }

    async fn reply(mock: &UdpSocket, to: SocketAddr, addr: &str, arg: OscType) {
        let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage { addr: addr.into(), args: vec![arg] })).unwrap();
        mock.send_to(&bytes, to).await.unwrap();
    }

    #[tokio::test]
    async fn an_incoming_headamp_float_is_decoded_to_decibels() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut adapter = X32Adapter::new("x32-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();
        let mut events = adapter.subscribe();
        let mut buf = [0u8; 512];
        let (_, from) = mock.recv_from(&mut buf).await.unwrap(); // the heartbeat

        reply(&mock, from, "/headamp/002/gain", OscType::Float(0.5)).await;
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv()).await.unwrap().unwrap();
        assert_eq!(event.address.channel, 3);
        assert_eq!(event.state.gain_db, 24.0);
        assert_eq!(event.changed, ChangedFields::GAIN);
    }

    /// The relative move reads the console's live trim first, then writes
    /// the result - so a trim someone changed at the desk is moved from where
    /// they left it.
    #[tokio::test]
    async fn nudge_trim_reads_the_live_value_then_writes_the_moved_one() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut adapter = X32Adapter::new("x32-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();

        let console = tokio::spawn(async move {
            let (get, from) = recv_ignoring_heartbeat(&mock).await;
            assert_eq!(get.addr, "/ch/07/preamp/trim");
            assert!(get.args.is_empty(), "first a read");
            reply(&mock, from, "/ch/07/preamp/trim", OscType::Float(TRIM_LAW.to_unit(-2.0))).await;
            let (set, _) = recv_ignoring_heartbeat(&mock).await;
            assert_eq!(set.addr, "/ch/07/preamp/trim");
            set.args
        });

        let applied = adapter.nudge_trim(7, -3.0).await.unwrap();
        assert_eq!(applied, -3.0);
        assert_eq!(console.await.unwrap(), vec![OscType::Float(TRIM_LAW.to_unit(-5.0))]);
    }

    #[tokio::test]
    async fn nudge_trim_reports_a_move_cut_short_at_the_limit() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut adapter = X32Adapter::new("x32-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();

        let console = tokio::spawn(async move {
            let (_, from) = recv_ignoring_heartbeat(&mock).await;
            reply(&mock, from, "/ch/01/preamp/trim", OscType::Float(TRIM_LAW.to_unit(17.0))).await;
            recv_ignoring_heartbeat(&mock).await.0.args
        });

        assert_eq!(adapter.nudge_trim(1, 3.0).await.unwrap(), 1.0);
        assert_eq!(console.await.unwrap(), vec![OscType::Float(1.0)]);
    }

    /// A console rebooting makes the adapter's reads fail ("connection
    /// refused" from the ICMP unreachable). That used to end the receive loop
    /// for good, so the desk was never heard again once it was back.
    #[tokio::test]
    async fn a_console_that_goes_away_and_comes_back_is_heard_again() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = mock.local_addr().unwrap();
        let mut adapter = X32Adapter::new("x32-1", addr);
        adapter.connect().await.unwrap();

        drop(mock); // the console goes away
        assert!(adapter.get_trim(1).await.is_err());

        let mock = UdpSocket::bind(addr).await.unwrap(); // and comes back
        let console = tokio::spawn(async move {
            let (get, from) = recv_ignoring_heartbeat(&mock).await;
            reply(&mock, from, &get.addr, OscType::Float(TRIM_LAW.to_unit(-4.0))).await;
            mock
        });
        assert_eq!(adapter.get_trim(1).await.unwrap(), -4.0);
        drop(console.await);
    }

    #[tokio::test]
    async fn a_move_under_half_a_step_costs_no_round_trip() {
        // Nothing is listening: a read would time out and fail.
        let mut adapter = X32Adapter::new("x32-1", "127.0.0.1:9".parse().unwrap());
        adapter.connect().await.unwrap();
        assert_eq!(adapter.nudge_trim(1, 0.1).await.unwrap(), 0.0);
    }

    #[test]
    fn parse_info_reply_infers_vendor_from_model_prefix() {
        let msg = OscMessage {
            addr: "/info".to_string(),
            args: vec![
                OscType::String("V2.05".into()),
                OscType::String("osc-server".into()),
                OscType::String("X32RACK".into()),
                OscType::String("2.12".into()),
            ],
        };
        let info = parse_info_reply(&msg, "10.0.0.5".parse().unwrap()).unwrap();
        assert_eq!(info.vendor, "Behringer");
        assert_eq!(info.model, "X32RACK");

        let midas = OscMessage {
            addr: "/info".to_string(),
            args: vec![
                OscType::String("V2.05".into()),
                OscType::String("osc-server".into()),
                OscType::String("M32C".into()),
                OscType::String("2.12".into()),
            ],
        };
        let info = parse_info_reply(&midas, "10.0.0.5".parse().unwrap()).unwrap();
        assert_eq!(info.vendor, "Midas");
    }

    #[tokio::test]
    async fn identify_resolves_from_info_reply() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let mut adapter = X32Adapter::new("x32-1", mock_addr);
        adapter.connect().await.unwrap();

        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (len, from) = mock.recv_from(&mut buf).await.unwrap();
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
            // First packet is either the /xremote heartbeat or /info -
            // reply to /info specifically, ignore anything else.
            if let OscPacket::Message(m) = &packet {
                if m.addr != "/info" {
                    let (len, _) = mock.recv_from(&mut buf).await.unwrap();
                    let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
                    assert!(matches!(&packet, OscPacket::Message(m) if m.addr == "/info"));
                }
            }
            let reply = OscPacket::Message(OscMessage {
                addr: "/info".to_string(),
                args: vec![
                    OscType::String("V2.05".into()),
                    OscType::String("osc-server".into()),
                    OscType::String("X32".into()),
                    OscType::String("2.12".into()),
                ],
            });
            let bytes = rosc::encoder::encode(&reply).unwrap();
            mock.send_to(&bytes, from).await.unwrap();
        });

        let info = adapter.identify().await.unwrap();
        assert_eq!(info.vendor, "Behringer");
        assert_eq!(info.model, "X32");
        server.abort();
    }

    #[tokio::test]
    async fn disconnect_stops_the_receive_loop() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut adapter = X32Adapter::new("x32-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();
        let mut events = adapter.subscribe();

        // Learn the adapter's ephemeral port from its /xremote heartbeat.
        let mut buf = [0u8; 512];
        let (_, from) = mock.recv_from(&mut buf).await.unwrap();

        adapter.disconnect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await; // let the loop actually exit

        let packet = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: "/headamp/000/gain".to_string(),
            args: vec![OscType::Float(0.5)],
        }))
        .unwrap();
        mock.send_to(&packet, from).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(result.is_err(), "must not receive events after disconnect");
    }
}

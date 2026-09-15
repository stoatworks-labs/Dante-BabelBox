//! [`DeviceAdapter`] for the Sennheiser evolution-wireless G3 receiver,
//! built on the [`crate::codec`] wire logic.
//!
//! One channel, whose "gain" is the receiver's analog **AF-out** level
//! (`-24..=+18` dB in 3 dB steps). Transport is the binary WSM protocol on
//! UDP 8133 (both directions use port 8133, so the adapter binds 8133 and
//! addresses the receiver there). A 2 s keepalive holds the receiver's
//! single master slot, which gates writes; see the crate docs on the WSM
//! conflict.
//!
//! The receiver pushes a fresh config packet whenever a config attribute
//! changes — including a front-panel AF-out change — so the receive loop
//! reports those as gain events, giving best-effort bidirectional behaviour.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    AdapterError, AdapterResult, ChangedFields, DeviceAdapter, DeviceInfo, PreampAddress,
    PreampEvent, PreampState,
};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::codec;

/// Resend the keepalive well inside the receiver's client-table timeout.
const KEEPALIVE_PERIOD: Duration = Duration::from_secs(2);
/// The G3 exposes a single audio output.
const CHANNEL: u16 = 1;
/// This client's 4-byte session token. Any distinct value works; this is the
/// one WSM uses, which is convenient when comparing captures.
const TOKEN: [u8; 4] = [0x7f, 0xac, 0xaa, 0xec];

pub struct Ewg3Adapter {
    id: Arc<str>,
    remote: SocketAddr,
    /// Local UDP port to bind. The real receiver always replies to port
    /// 8133 regardless of our source port, so production must bind 8133;
    /// tests bind 0 (ephemeral) against a localhost mock that replies to
    /// whatever source port it sees.
    bind_port: u16,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<Option<PreampState>>>,
    cancel: CancellationToken,
}

impl Ewg3Adapter {
    /// `device` is the receiver's IP (from its front-panel IP-Address menu).
    pub fn new(id: impl Into<Arc<str>>, device: Ipv4Addr) -> Self {
        Self::with_endpoints(id, SocketAddr::new(IpAddr::V4(device), codec::PORT), codec::PORT)
    }

    /// Explicit endpoints, for tests against a localhost mock receiver.
    pub fn with_endpoints(id: impl Into<Arc<str>>, remote: SocketAddr, bind_port: u16) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote,
            bind_port,
            socket: None,
            tx,
            state: Arc::new(Mutex::new(None)),
            cancel: CancellationToken::new(),
        }
    }

    fn socket(&self) -> AdapterResult<&Arc<UdpSocket>> {
        self.socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))
    }

    async fn send(&self, packet: &[u8]) -> AdapterResult<()> {
        self.socket()?
            .send_to(packet, self.remote)
            .await
            .map(|_| ())
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }
}

/// Bind a UDP socket to `0.0.0.0:<port>` with address/port reuse so it can
/// coexist with other 8133 listeners on the host.
fn bind_reuse(port: u16) -> io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    // set_reuse_port is unix-only; the workspace targets macOS/Linux.
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    let addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    sock.bind(&addr.into())?;
    UdpSocket::from_std(sock.into())
}

#[async_trait]
impl DeviceAdapter for Ewg3Adapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let socket = bind_reuse(self.bind_port).map_err(|e| AdapterError::Connection(e.to_string()))?;
        let socket = Arc::new(socket);
        self.socket = Some(Arc::clone(&socket));

        spawn_receive_loop(
            Arc::clone(&socket),
            Arc::clone(&self.id),
            self.remote.ip(),
            self.tx.clone(),
            Arc::clone(&self.state),
            self.cancel.clone(),
        );
        spawn_keepalive_loop(Arc::clone(&socket), self.remote, self.cancel.clone());

        // Prime the master slot and pull initial state.
        self.send(&codec::register(TOKEN, true)).await?;
        self.send(&codec::config_request(TOKEN)).await?;
        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        self.socket = None;
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        // Best effort: pull a config packet so the model name reflects the
        // receiver's own name; fall back to a generic label.
        self.send(&codec::config_request(TOKEN)).await.ok();
        Ok(DeviceInfo {
            vendor: "Sennheiser".into(),
            model: "evolution wireless G3".into(),
            address: self.remote.ip(),
        })
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        if channel != CHANNEL {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        let clamped = gain_db.clamp(codec::AF_MIN_DB, codec::AF_MAX_DB);
        debug!(%self.id, gain_db, clamped, "set AF-out");
        self.send(&codec::set_af_out_db(TOKEN, clamped)).await?;
        // Optimistically cache; the receiver's config push will confirm and
        // drive the event (the Router echo-suppresses that by value).
        let mut st = self.state.lock().await;
        let mut s = st.unwrap_or(PreampState { gain_db: 0.0, phantom: false, pad: None });
        s.gain_db = codec::af_index_to_db(codec::af_db_to_index(clamped));
        *st = Some(s);
        Ok(())
    }

    async fn set_phantom(&mut self, channel: u16, _on: bool) -> AdapterResult<()> {
        if channel != CHANNEL {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        // The G3 has no remote phantom power. Accept and ignore so a mapping
        // that happens to carry phantom doesn't error the whole bridge.
        debug!(%self.id, "set_phantom ignored (G3 has no remote phantom)");
        Ok(())
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if channel != CHANNEL {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        if let Some(state) = *self.state.lock().await {
            return Ok(state);
        }
        self.send(&codec::config_request(TOKEN)).await?;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = *self.state.lock().await {
                return Ok(state);
            }
        }
        Err(AdapterError::Protocol(
            "no config reply from receiver (is another client — e.g. WSM — \
             holding the master slot?)"
                .into(),
        ))
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

fn spawn_keepalive_loop(socket: Arc<UdpSocket>, remote: SocketAddr, cancel: CancellationToken) {
    tokio::spawn(async move {
        let packet = codec::register(TOKEN, true);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(KEEPALIVE_PERIOD) => {
                    if let Err(e) = socket.send_to(&packet, remote).await {
                        warn!(error = %e, "G3 keepalive send failed");
                    }
                }
            }
        }
    });
}

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    device_ip: IpAddr,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<Option<PreampState>>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let (n, from) = tokio::select! {
                _ = cancel.cancelled() => break,
                r = socket.recv_from(&mut buf) => match r {
                    Ok(v) => v,
                    Err(e) => { warn!(error = %e, "G3 recv failed"); continue; }
                }
            };
            // Only trust packets from our receiver.
            if from.ip() != device_ip {
                continue;
            }
            let pkt = &buf[..n];
            if pkt.len() < 4 || pkt[..4] != codec::HDR_CONFIG {
                continue; // meters / acks / discovery — not needed here
            }
            let Ok(cfg) = codec::Config::parse(pkt) else { continue };
            let new_state = PreampState { gain_db: cfg.af_out_db, phantom: false, pad: None };
            let mut guard = state.lock().await;
            let changed = guard.map(|s| s.gain_db != new_state.gain_db).unwrap_or(true);
            *guard = Some(new_state);
            drop(guard);
            if changed {
                debug!(%id, af_out_db = cfg.af_out_db, "AF-out changed on device");
                let _ = tx.send(PreampEvent {
                    address: PreampAddress::new(id.to_string(), CHANNEL),
                    state: new_state,
                    changed: ChangedFields::GAIN,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::UdpSocket as TokioUdp;

    /// A localhost mock standing in for the receiver: replies to whatever
    /// source port the adapter used, so no fixed 8133 bind is needed.
    async fn mock() -> (TokioUdp, SocketAddr) {
        let s = TokioUdp::bind("127.0.0.1:0").await.unwrap();
        let a = s.local_addr().unwrap();
        (s, a)
    }

    #[tokio::test]
    async fn set_gain_puts_a_correct_af_out_write_on_the_wire() {
        let (server, server_addr) = mock().await;
        let mut adapter = Ewg3Adapter::with_endpoints("g3", server_addr, 0);
        adapter.connect().await.unwrap();
        adapter.set_gain(1, 6.0).await.unwrap();

        // connect() sends register + config_request; then the AF-out write.
        let mut buf = [0u8; 2048];
        let mut saw_write = false;
        for _ in 0..10 {
            let Ok(Ok((n, _))) =
                tokio::time::timeout(Duration::from_millis(500), server.recv_from(&mut buf)).await
            else {
                break;
            };
            if buf[..4] == [0xb1, 0xf8, 0xf7, 0xca] {
                let body = &buf[8..n];
                // +6 dB -> index 10; only AF-out + its flags set.
                assert_eq!(body[0x0e], codec::af_db_to_index(6.0));
                assert_eq!(body[0x0e], 10);
                assert_eq!(body[0x4e], 1);
                assert_eq!(body[0x45 + 0x0e], 1);
                saw_write = true;
                break;
            }
        }
        assert!(saw_write, "adapter never sent an AF-out write");
    }

    #[tokio::test]
    async fn unsupported_channel_is_rejected() {
        let (_server, server_addr) = mock().await;
        let mut adapter = Ewg3Adapter::with_endpoints("g3", server_addr, 0);
        adapter.connect().await.unwrap();
        assert!(matches!(
            adapter.set_gain(2, 0.0).await,
            Err(AdapterError::UnsupportedChannel(2))
        ));
    }

    #[tokio::test]
    async fn a_device_config_push_surfaces_as_a_gain_event() {
        let (server, server_addr) = mock().await;
        let mut adapter = Ewg3Adapter::with_endpoints("g3", server_addr, 0);
        let mut events = adapter.subscribe();
        adapter.connect().await.unwrap();

        // Learn the adapter's source addr from its first packet, then push a
        // config packet reporting AF-out = +6 dB (index 10).
        let mut buf = [0u8; 2048];
        let (_n, adapter_src) = server.recv_from(&mut buf).await.unwrap();
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&codec::HDR_CONFIG);
        pkt.push(0x01);
        pkt.extend_from_slice(&[0x00, 0x1b, 0x66, 0x7a, 0xde, 0x44]);
        pkt.push(0x01);
        let mut body = vec![0u8; 0x11];
        body[..8].copy_from_slice(b"ew500 G3");
        body[8..12].copy_from_slice(&645450u32.to_le_bytes());
        body[0x0e] = 10; // +6 dB
        pkt.extend_from_slice(&body);
        server.send_to(&pkt, adapter_src).await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("no event within 1s")
            .expect("event channel closed");
        assert_eq!(event.address.channel, 1);
        assert_eq!(event.state.gain_db, 6.0);
        assert_eq!(event.changed, ChangedFields::GAIN);
    }
}

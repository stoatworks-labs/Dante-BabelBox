//! [`DeviceAdapter`] for RME TotalMix FX, over its Global OSC.
//!
//! Each channel is a TotalMix hardware output, numbered from 1 as TotalMix
//! labels them, and its "gain" is that output's fader in dB (-65..=+6, where
//! -65 is off). A stereo pair is addressed by its left channel, as TotalMix
//! does. There is no phantom power: `set_phantom` is accepted and ignored,
//! as the G3 adapter does, so a mapping that carries phantom can't error the
//! bridge.
//!
//! One UDP socket does both directions. It sends to TotalMix's incoming port
//! (7001 by default) and is bound to the port TotalMix replies to (its
//! "Port outgoing", 9001 by default), so TotalMix's "Remote IP" must be this
//! machine. TotalMix answers one remote only: anything else bound to 9001,
//! such as Companion's TotalMix module, has to move or stop.
//!
//! What goes on the wire, all of it from the Companion module (see
//! [`crate`]):
//! - `/output/<index>/faderlin <float>` sets an output fader, `index` 0-based;
//! - `/sendchan/output/<index> 1.0` asks for one output's state;
//! - `/sendstate 1.0` asks for the status block, `/status/device` among it.
//!
//! TotalMix reports a fader as `/output/<index>/faderlin` (0..1) or
//! `/output/<index>/volume` (dB). Either becomes a gain event, whoever moved
//! the fader, unless it only confirms the position this adapter last wrote.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    AdapterError, AdapterResult, ChangedFields, DeviceAdapter, DeviceInfo, PreampAddress,
    PreampEvent, PreampState,
};
use rosc::{OscMessage, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::law;

/// TotalMix's default incoming OSC port.
pub const TOTALMIX_PORT: u16 = 7001;
/// TotalMix's default outgoing port, where its replies arrive.
pub const REPLY_PORT: u16 = 9001;

const READ_ATTEMPTS: usize = 20;
const READ_POLL: Duration = Duration::from_millis(50);

/// What the receive loop keeps current.
#[derive(Default)]
struct Seen {
    /// Fader position per channel (1-based).
    positions: HashMap<u16, u16>,
    /// `/status/device`, once TotalMix has said.
    device: Option<String>,
}

pub struct TotalMixAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    bind_port: u16,
    outputs: u16,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    seen: Arc<StdMutex<Seen>>,
    cancel: CancellationToken,
}

impl TotalMixAdapter {
    /// TotalMix at `totalmix` (its incoming port), with `outputs` hardware
    /// outputs exposed as channels 1..=outputs. Replies are read on 9001.
    pub fn new(id: impl Into<Arc<str>>, totalmix: SocketAddr, outputs: u16) -> Self {
        Self::with_endpoints(id, totalmix, REPLY_PORT, outputs)
    }

    /// Explicit reply port, for tests against a localhost stand-in (0 binds
    /// an ephemeral port, and the stand-in replies to whatever it sees).
    pub fn with_endpoints(
        id: impl Into<Arc<str>>,
        totalmix: SocketAddr,
        bind_port: u16,
        outputs: u16,
    ) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote: totalmix,
            bind_port,
            outputs,
            socket: None,
            tx,
            seen: Arc::new(StdMutex::new(Seen::default())),
            cancel: CancellationToken::new(),
        }
    }

    /// TotalMix's 0-based output index for `channel`.
    fn index(&self, channel: u16) -> AdapterResult<u16> {
        if channel == 0 || channel > self.outputs {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(channel - 1)
    }

    async fn send(&self, addr: String, args: Vec<OscType>) -> AdapterResult<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage { addr, args }))
            .map_err(|e| AdapterError::Protocol(format!("OSC encode failed: {e:?}")))?;
        socket
            .send_to(&bytes, self.remote)
            .await
            .map(|_| ())
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }

    fn position(&self, channel: u16) -> Option<u16> {
        self.seen.lock().unwrap().positions.get(&channel).copied()
    }

    fn state_at(position: u16) -> PreampState {
        PreampState {
            gain_db: law::db_for_position(position),
            phantom: false,
            pad: None,
        }
    }
}

#[async_trait]
impl DeviceAdapter for TotalMixAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        // A reconnect needs a live token: the last disconnect cancelled the old one.
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.bind_port);
        let socket = UdpSocket::bind(bind).await.map_err(|e| {
            AdapterError::Connection(format!(
                "can't listen for TotalMix on UDP {}: {e} (is something else, such as \
                 Companion's TotalMix module, already on that port?)",
                self.bind_port
            ))
        })?;
        let socket = Arc::new(socket);
        self.socket = Some(Arc::clone(&socket));
        // Faders may have moved while nothing was listening.
        self.seen.lock().unwrap().positions.clear();
        spawn_receive_loop(
            socket,
            Arc::clone(&self.id),
            self.remote.ip(),
            self.outputs,
            self.tx.clone(),
            Arc::clone(&self.seen),
            self.cancel.clone(),
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        self.socket = None;
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        // Best effort: the device name comes from TotalMix's status block.
        self.send("/sendstate".into(), vec![OscType::Float(1.0)])
            .await?;
        let mut model = None;
        for _ in 0..READ_ATTEMPTS {
            model = self.seen.lock().unwrap().device.clone();
            if model.is_some() {
                break;
            }
            tokio::time::sleep(READ_POLL).await;
        }
        Ok(DeviceInfo {
            vendor: "RME".into(),
            model: model.unwrap_or_else(|| "TotalMix FX".into()),
            address: self.remote.ip(),
        })
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let index = self.index(channel)?;
        let position = law::position_for_db(gain_db);
        debug!(%self.id, channel, gain_db, position, "set output fader");
        self.send(
            format!("/output/{index}/faderlin"),
            vec![OscType::Float(law::faderlin(position))],
        )
        .await?;
        // Cache the position written, so TotalMix's confirmation of it is no news.
        self.seen
            .lock()
            .unwrap()
            .positions
            .insert(channel, position);
        Ok(())
    }

    async fn set_phantom(&mut self, channel: u16, _on: bool) -> AdapterResult<()> {
        self.index(channel)?;
        debug!(%self.id, "set_phantom ignored (a TotalMix output has no phantom power)");
        Ok(())
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        let index = self.index(channel)?;
        if let Some(position) = self.position(channel) {
            return Ok(Self::state_at(position));
        }
        self.send(
            format!("/sendchan/output/{index}"),
            vec![OscType::Float(1.0)],
        )
        .await?;
        for _ in 0..READ_ATTEMPTS {
            tokio::time::sleep(READ_POLL).await;
            if let Some(position) = self.position(channel) {
                return Ok(Self::state_at(position));
            }
        }
        Err(AdapterError::Protocol(format!(
            "TotalMix didn't report output {channel}. Check that OSC is on in TotalMix's \
             settings, its Remote IP is this machine and its outgoing port is {}",
            self.bind_port
        )))
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

/// A fader report: channel (1-based) and position.
fn fader_report(msg: &OscMessage, outputs: u16) -> Option<(u16, u16)> {
    let rest = msg.addr.strip_prefix("/output/")?;
    let (index, field) = rest.split_once('/')?;
    let index: u16 = index.parse().ok()?;
    let channel = index.checked_add(1).filter(|c| *c <= outputs)?;
    let value = match msg.args.first()? {
        OscType::Float(v) => *v as f64,
        OscType::Double(v) => *v,
        OscType::Int(v) => *v as f64,
        _ => return None,
    };
    let position = match field {
        "faderlin" => law::position_for_faderlin(value as f32),
        "volume" => law::position_for_db(value as f32),
        _ => return None,
    };
    Some((channel, position))
}

fn messages(packet: OscPacket, out: &mut Vec<OscMessage>) {
    match packet {
        OscPacket::Message(m) => out.push(m),
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| messages(p, out)),
    }
}

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    totalmix_ip: IpAddr,
    outputs: u16,
    tx: broadcast::Sender<PreampEvent>,
    seen: Arc<StdMutex<Seen>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        loop {
            let (n, from) = tokio::select! {
                _ = cancel.cancelled() => break,
                r = socket.recv_from(&mut buf) => match r {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "TotalMix recv failed");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                }
            };
            if from.ip() != totalmix_ip {
                continue;
            }
            let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) else {
                continue;
            };
            let mut batch = Vec::new();
            messages(packet, &mut batch);
            for msg in batch {
                if msg.addr == "/status/device" {
                    if let Some(OscType::String(name)) = msg.args.first() {
                        seen.lock().unwrap().device = Some(name.clone());
                    }
                    continue;
                }
                let Some((channel, position)) = fader_report(&msg, outputs) else {
                    continue;
                };
                let before = seen.lock().unwrap().positions.insert(channel, position);
                if before == Some(position) {
                    continue;
                }
                let state = TotalMixAdapter::state_at(position);
                debug!(%id, channel, gain_db = state.gain_db, "output fader moved in TotalMix");
                let _ = tx.send(PreampEvent {
                    address: PreampAddress::new(id.to_string(), channel),
                    state,
                    changed: ChangedFields::GAIN,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A localhost stand-in for TotalMix. It replies to whatever port the
    /// adapter sent from, so no fixed 9001 bind is needed.
    async fn stand_in() -> (UdpSocket, SocketAddr) {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let a = s.local_addr().unwrap();
        (s, a)
    }

    async fn connected(outputs: u16) -> (TotalMixAdapter, UdpSocket) {
        let (server, addr) = stand_in().await;
        let mut adapter = TotalMixAdapter::with_endpoints("tm", addr, 0, outputs);
        adapter.connect().await.unwrap();
        (adapter, server)
    }

    async fn recv(server: &UdpSocket) -> (OscMessage, SocketAddr) {
        let mut buf = [0u8; 2048];
        let (n, from) = tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buf))
            .await
            .expect("nothing reached the stand-in within 1 s")
            .unwrap();
        let (_, packet) = rosc::decoder::decode_udp(&buf[..n]).unwrap();
        let OscPacket::Message(m) = packet else {
            panic!("expected a message")
        };
        (m, from)
    }

    async fn reply(server: &UdpSocket, to: SocketAddr, addr: &str, args: Vec<OscType>) {
        let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: addr.into(),
            args,
        }))
        .unwrap();
        server.send_to(&bytes, to).await.unwrap();
    }

    fn float(m: &OscMessage) -> f32 {
        match m.args.as_slice() {
            [OscType::Float(v)] => *v,
            other => panic!("expected one float, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_gain_moves_the_output_fader_by_its_0_based_index() {
        let (mut adapter, server) = connected(16).await;
        adapter.set_gain(5, 0.0).await.unwrap();
        let (m, _) = recv(&server).await;
        assert_eq!(m.addr, "/output/4/faderlin");
        assert_eq!(float(&m), 836.0 / 1023.0);
    }

    #[tokio::test]
    async fn set_gain_clamps_to_totalmix_range() {
        let (mut adapter, server) = connected(2).await;
        adapter.set_gain(1, 30.0).await.unwrap();
        assert_eq!(float(&recv(&server).await.0), 1.0);
        adapter.set_gain(2, -120.0).await.unwrap();
        assert_eq!(float(&recv(&server).await.0), 0.0);
    }

    #[tokio::test]
    async fn channels_outside_1_to_outputs_are_rejected() {
        let (mut adapter, _server) = connected(8).await;
        assert!(matches!(
            adapter.set_gain(0, 0.0).await,
            Err(AdapterError::UnsupportedChannel(0))
        ));
        assert!(matches!(
            adapter.set_gain(9, 0.0).await,
            Err(AdapterError::UnsupportedChannel(9))
        ));
        assert!(matches!(
            adapter.get_state(9).await,
            Err(AdapterError::UnsupportedChannel(9))
        ));
    }

    #[tokio::test]
    async fn a_fader_moved_in_totalmix_is_a_gain_event() {
        let (mut adapter, server) = connected(8).await;
        let mut events = adapter.subscribe();
        // Learn the adapter's port from a first message, then report output 3.
        adapter.set_gain(1, -10.0).await.unwrap();
        let (_, adapter_addr) = recv(&server).await;
        reply(
            &server,
            adapter_addr,
            "/output/2/faderlin",
            vec![OscType::Float(836.0 / 1023.0)],
        )
        .await;

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("no event within 1 s")
            .unwrap();
        assert_eq!(event.address, PreampAddress::new("tm", 3));
        assert_eq!(event.changed, ChangedFields::GAIN);
        assert!(event.state.gain_db.abs() < 0.02, "{}", event.state.gain_db);
    }

    #[tokio::test]
    async fn a_volume_report_in_db_is_read_too() {
        let (mut adapter, server) = connected(8).await;
        let mut events = adapter.subscribe();
        adapter.set_gain(1, -10.0).await.unwrap();
        let (_, adapter_addr) = recv(&server).await;
        reply(
            &server,
            adapter_addr,
            "/output/7/volume",
            vec![OscType::Float(-20.0)],
        )
        .await;

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("no event within 1 s")
            .unwrap();
        assert_eq!(event.address.channel, 8);
        assert!(
            (event.state.gain_db - -20.0).abs() < 0.1,
            "{}",
            event.state.gain_db
        );
    }

    #[tokio::test]
    async fn totalmix_confirming_our_own_write_is_not_an_event() {
        let (mut adapter, server) = connected(8).await;
        let mut events = adapter.subscribe();
        adapter.set_gain(2, -3.0).await.unwrap();
        let (m, adapter_addr) = recv(&server).await;
        reply(&server, adapter_addr, &m.addr, m.args.clone()).await;
        let got = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        assert!(
            got.is_err(),
            "an echo of our own write came back as an event: {got:?}"
        );
    }

    #[tokio::test]
    async fn reports_for_outputs_beyond_the_configured_count_are_ignored() {
        let (mut adapter, server) = connected(4).await;
        let mut events = adapter.subscribe();
        adapter.set_gain(1, -10.0).await.unwrap();
        let (_, adapter_addr) = recv(&server).await;
        reply(
            &server,
            adapter_addr,
            "/output/4/faderlin",
            vec![OscType::Float(0.5)],
        )
        .await;
        let got = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        assert!(got.is_err(), "output 5 of 4 came back as an event: {got:?}");
    }

    #[tokio::test]
    async fn get_state_asks_totalmix_for_an_unread_output() {
        let (mut adapter, server) = connected(8).await;
        let ask = tokio::spawn(async move {
            let (m, from) = recv(&server).await;
            assert_eq!(m.addr, "/sendchan/output/5");
            reply(
                &server,
                from,
                "/output/5/faderlin",
                vec![OscType::Float(649.0 / 1023.0)],
            )
            .await;
            server
        });
        let state = adapter.get_state(6).await.unwrap();
        ask.await.unwrap();
        assert!((state.gain_db - -6.0).abs() < 0.01, "{}", state.gain_db);
        assert!(!state.phantom);
    }

    #[tokio::test]
    async fn identify_takes_the_device_name_from_the_status_block() {
        let (mut adapter, server) = connected(8).await;
        let answer = tokio::spawn(async move {
            let (m, from) = recv(&server).await;
            assert_eq!(m.addr, "/sendstate");
            reply(
                &server,
                from,
                "/status/device",
                vec![OscType::String("Digiface Dante".into())],
            )
            .await;
            server
        });
        let info = adapter.identify().await.unwrap();
        answer.await.unwrap();
        assert_eq!(info.vendor, "RME");
        assert_eq!(info.model, "Digiface Dante");
    }

    #[tokio::test]
    async fn it_reconnects_after_a_disconnect() {
        let (mut adapter, server) = connected(8).await;
        adapter.disconnect().await.unwrap();
        assert!(adapter.set_gain(1, 0.0).await.is_err());
        adapter.connect().await.unwrap();
        let mut events = adapter.subscribe();
        adapter.set_gain(1, -10.0).await.unwrap();
        let (_, adapter_addr) = recv(&server).await;
        reply(
            &server,
            adapter_addr,
            "/output/0/faderlin",
            vec![OscType::Float(0.5)],
        )
        .await;
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("the receive loop didn't come back after reconnecting")
            .unwrap();
        assert_eq!(event.address.channel, 1);
    }

    #[tokio::test]
    async fn phantom_is_accepted_and_ignored() {
        let (mut adapter, server) = connected(8).await;
        adapter.set_phantom(1, true).await.unwrap();
        let mut buf = [0u8; 64];
        let sent =
            tokio::time::timeout(Duration::from_millis(200), server.recv_from(&mut buf)).await;
        assert!(sent.is_err(), "set_phantom put something on the wire");
    }
}

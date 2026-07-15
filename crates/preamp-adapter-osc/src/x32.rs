//! `DeviceAdapter` for the X32-family OSC dialect: Behringer X32, Midas M32
//! and Midas HD96 all share this protocol (M32/HD96 are the same firmware
//! lineage as X32 under a different badge). Source: the widely-used
//! "Unofficial X32/M32 OSC Remote Protocol" spec (x32ram.com).
//!
//! Wire format: plain OSC over UDP, default port 10023. Headamp index 1-24
//! (1-8 local XLR, 9-16 AES50-A, 17-24 AES50-B), zero-padded two digits:
//! `/headamp/<01-24>/gain` (float, -12..60 dB) and `/headamp/<01-24>/phantom`
//! (int 0/1). Sending an address with no argument is a "get" - the console
//! replies with the same address and its current value. The console only
//! proactively pushes value-changed notifications (e.g. someone turning a
//! physical/on-screen gain knob) to clients that keep sending `/xremote`
//! at least every ~10s, so `connect()` starts that heartbeat.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_core::{
    AdapterError, AdapterResult, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent, PreampState,
};
use rosc::{OscMessage, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub struct X32Adapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
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
            pending_identify: Arc::new(StdMutex::new(None)),
            cancel: CancellationToken::new(),
        }
    }

    fn headamp_path(channel: u16, suffix: &str) -> AdapterResult<String> {
        if !(1..=24).contains(&channel) {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(format!("/headamp/{channel:02}/{suffix}"))
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
            Arc::clone(&self.state),
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
        self.send(&addr, vec![OscType::Float(gain_db.clamp(-12.0, 60.0))])
            .await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let addr = Self::headamp_path(channel, "phantom")?;
        self.send(&addr, vec![OscType::Int(on as i32)]).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        self.send(&Self::headamp_path(channel, "gain")?, vec![]).await?;
        self.send(&Self::headamp_path(channel, "phantom")?, vec![]).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
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
}

#[derive(Debug, Clone, Copy)]
enum HeadampField {
    Gain,
    Phantom,
}

fn parse_headamp_addr(addr: &str) -> Option<(u16, HeadampField)> {
    let rest = addr.strip_prefix("/headamp/")?;
    let (chan_str, field_str) = rest.split_once('/')?;
    let channel: u16 = chan_str.parse().ok()?;
    let field = match field_str {
        "gain" => HeadampField::Gain,
        "phantom" => HeadampField::Phantom,
        _ => return None,
    };
    Some((channel, field))
}

type PendingIdentify = Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>;

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
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
                        warn!(device = %id, error = %e, "X32 OSC socket read failed, stopping receive loop");
                        return;
                    }
                },
            };
            match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, packet)) => {
                    handle_packet(packet, &id, &tx, &state, &pending_identify, remote_ip).await
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
    state: &'a Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: &'a PendingIdentify,
    remote_ip: IpAddr,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        match packet {
            OscPacket::Message(msg) => {
                handle_message(msg, id, tx, state, pending_identify, remote_ip).await
            }
            OscPacket::Bundle(bundle) => {
                for p in bundle.content {
                    handle_packet(p, id, tx, state, pending_identify, remote_ip).await;
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
    state: &Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: &PendingIdentify,
    remote_ip: IpAddr,
) {
    if let Some(info) = parse_info_reply(&msg, remote_ip) {
        if let Some(sender) = pending_identify.lock().unwrap().take() {
            let _ = sender.send(info);
        }
        return;
    }

    let Some((channel, field)) = parse_headamp_addr(&msg.addr) else {
        return;
    };
    let value = match msg.args.first() {
        Some(OscType::Float(f)) => *f,
        Some(OscType::Int(i)) => *i as f32,
        _ => return,
    };

    let new_state = {
        let mut guard = state.lock().await;
        let entry = guard.entry(channel).or_insert(PreampState {
            gain_db: 0.0,
            phantom: false,
            pad: None,
        });
        match field {
            HeadampField::Gain => entry.gain_db = value,
            HeadampField::Phantom => entry.phantom = value != 0.0,
        }
        *entry
    };

    debug!(device = %id, channel, ?field, value, "X32 headamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
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
                    if let Err(e) = socket.send(&bytes).await {
                        warn!(error = %e, "failed to send /xremote heartbeat, stopping");
                        return;
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
    fn headamp_path_zero_pads_and_rejects_out_of_range() {
        assert_eq!(X32Adapter::headamp_path(1, "gain").unwrap(), "/headamp/01/gain");
        assert_eq!(X32Adapter::headamp_path(24, "phantom").unwrap(), "/headamp/24/phantom");
        assert!(X32Adapter::headamp_path(0, "gain").is_err());
        assert!(X32Adapter::headamp_path(25, "gain").is_err());
    }

    #[test]
    fn parse_headamp_addr_round_trips_known_paths() {
        assert!(matches!(
            parse_headamp_addr("/headamp/01/gain"),
            Some((1, HeadampField::Gain))
        ));
        assert!(matches!(
            parse_headamp_addr("/headamp/24/phantom"),
            Some((24, HeadampField::Phantom))
        ));
        assert!(parse_headamp_addr("/ch/01/mix/fader").is_none());
        assert!(parse_headamp_addr("/headamp/01/pad").is_none());
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
            addr: "/headamp/01/gain".to_string(),
            args: vec![OscType::Float(10.0)],
        }))
        .unwrap();
        mock.send_to(&packet, from).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(result.is_err(), "must not receive events after disconnect");
    }
}

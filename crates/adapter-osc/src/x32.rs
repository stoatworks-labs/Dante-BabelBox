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
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use preamp_bridge_core::{
    AdapterError, AdapterResult, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent, PreampState,
};
use rosc::{OscMessage, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

pub struct X32Adapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
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
        );
        spawn_xremote_heartbeat(Arc::clone(&socket));

        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        // TODO: the receive loop only turns headamp messages into
        // PreampEvents right now. Answering /info properly needs a
        // one-shot request/reply path (e.g. a oneshot channel keyed by
        // OSC address) rather than the broadcast-only design below.
        Err(AdapterError::Protocol(
            "identify: /info reply handling not yet implemented".into(),
        ))
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

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let len = match socket.recv(&mut buf).await {
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "X32 OSC socket read failed, stopping receive loop");
                    return;
                }
            };
            match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, packet)) => handle_packet(packet, &id, &tx, &state).await,
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
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        match packet {
            OscPacket::Message(msg) => handle_message(msg, id, tx, state).await,
            OscPacket::Bundle(bundle) => {
                for p in bundle.content {
                    handle_packet(p, id, tx, state).await;
                }
            }
        }
    })
}

async fn handle_message(
    msg: OscMessage,
    id: &Arc<str>,
    tx: &broadcast::Sender<PreampEvent>,
    state: &Arc<Mutex<HashMap<u16, PreampState>>>,
) {
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

fn spawn_xremote_heartbeat(socket: Arc<UdpSocket>) {
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
            tokio::time::sleep(Duration::from_secs(9)).await;
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
}

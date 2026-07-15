//! `DeviceAdapter` for Behringer Wing, built from the official "OSC
//! Documentation for WING" (Patrick-Gilles Maillot, authorized by
//! Behringer, wing-docs.com, v0.3.2).
//!
//! Wire format: plain OSC over UDP, port 2223 (WING also has a distinct
//! "native" TCP/UDP protocol on port 2222 - not this one). WING does not
//! support OSC bundles or address wildcards - one message per packet.
//!
//! Headamp gain/phantom live under the `io.in.<group>.<n>` JSON subtree as
//! plain fields `g` (dB float, no 0-127 scaling unlike X32) and `vph`
//! (bool). WING has several source groups (LCL, A, B, C, SC, USB, CRD,
//! PLAY, AES, USR, OSC) - this adapter only covers `LCL` (the console's 8
//! built-in MIDAS PRO preamps, `/io/in/LCL/<1-8>/...`), since that's the
//! unambiguous case directly documented. AES50 (A/B) and StageConnect (SC)
//! connected stageboxes use the same field names but their channel
//! count/ordering isn't spelled out precisely enough here to map safely -
//! extend `channel_path` once that's confirmed against real hardware.
//!
//! WING only pushes unsolicited value-changed messages (e.g. a physical
//! gain knob turn) to a client that holds an active OSC subscription
//! (`/*s`), and only one subscriber is allowed globally - a second app
//! (e.g. WING's own control app) subscribing will silently displace ours.
//! Subscriptions expire after 10s, so `connect()` renews every 8s.
//!
//! Get-reply argument layout for a given address isn't fully spelled out
//! for the `g`/`vph` fields specifically (only shown for `fdr` and `mute`
//! in the docs, which both trail with the actual value as their last
//! float/int argument). This adapter takes the last float or int argument
//! in a reply as the value, which matches every worked example in the
//! spec and degrades gracefully if WING replies with more or fewer
//! leading fields than expected.

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

pub struct WingAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>,
    cancel: CancellationToken,
}

impl WingAdapter {
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

    fn channel_path(channel: u16, field: &str) -> AdapterResult<String> {
        if !(1..=8).contains(&channel) {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(format!("/io/in/LCL/{channel}/{field}"))
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
impl DeviceAdapter for WingAdapter {
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
        spawn_subscription_renewal(Arc::clone(&socket), self.cancel.clone());

        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        let (tx, rx) = oneshot::channel();
        *self.pending_identify.lock().unwrap() = Some(tx);
        self.send("/?", vec![]).await?;
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(|_| AdapterError::Protocol("identify: timed out waiting for /? reply".into()))?
            .map_err(|_| AdapterError::Protocol("identify: reply channel dropped".into()))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let addr = Self::channel_path(channel, "g")?;
        self.send(&addr, vec![OscType::Float(gain_db)]).await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let addr = Self::channel_path(channel, "vph")?;
        self.send(&addr, vec![OscType::Int(on as i32)]).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        self.send(&Self::channel_path(channel, "g")?, vec![]).await?;
        self.send(&Self::channel_path(channel, "vph")?, vec![]).await?;

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
    let rest = addr.strip_prefix("/io/in/LCL/")?;
    let (chan_str, field_str) = rest.split_once('/')?;
    let channel: u16 = chan_str.parse().ok()?;
    let field = match field_str {
        "g" => HeadampField::Gain,
        "vph" => HeadampField::Phantom,
        _ => return None,
    };
    Some((channel, field))
}

/// Per the module doc comment, takes the last float or int argument as the
/// value - matches every worked reply example in the spec (`fdr`, `mute`).
fn last_numeric_value(args: &[OscType]) -> Option<f32> {
    args.iter().rev().find_map(|a| match a {
        OscType::Float(f) => Some(*f),
        OscType::Int(i) => Some(*i as f32),
        _ => None,
    })
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
        let mut buf = [0u8; 8192];
        loop {
            let len = tokio::select! {
                _ = cancel.cancelled() => return,
                result = socket.recv(&mut buf) => match result {
                    Ok(len) => len,
                    Err(e) => {
                        warn!(device = %id, error = %e, "WING OSC socket read failed, stopping receive loop");
                        return;
                    }
                },
            };
            match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, OscPacket::Message(msg))) => {
                    handle_message(msg, &id, &tx, &state, &pending_identify, remote_ip).await
                }
                Ok((_, OscPacket::Bundle(_))) => {
                    debug!(device = %id, "ignoring unexpected OSC bundle (WING doesn't send these)");
                }
                Err(e) => debug!(device = %id, error = ?e, "dropping malformed OSC packet"),
            }
        }
    });
}

/// Parses the `/?` reply: single string `"WING,<ip>,<name>,<model>,<serial>,<firmware>"`
/// (from the official WING OSC PDF's "WING OSC Messages" section).
fn parse_info_reply(msg: &OscMessage, address: IpAddr) -> Option<DeviceInfo> {
    if msg.addr != "/?" {
        return None;
    }
    let OscType::String(s) = msg.args.first()? else {
        return None;
    };
    let parts: Vec<&str> = s.split(',').collect();
    if parts.first() != Some(&"WING") {
        return None;
    }
    let model = parts.get(3)?.to_string();
    Some(DeviceInfo {
        vendor: "Behringer".to_string(),
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
    let Some(value) = last_numeric_value(&msg.args) else {
        return;
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

    debug!(device = %id, channel, ?field, value, "WING headamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
    });
}

fn spawn_subscription_renewal(socket: Arc<UdpSocket>, cancel: CancellationToken) {
    tokio::spawn(async move {
        loop {
            let packet = OscPacket::Message(OscMessage {
                addr: "/*s".to_string(),
                args: vec![],
            });
            match rosc::encoder::encode(&packet) {
                Ok(bytes) => {
                    if let Err(e) = socket.send(&bytes).await {
                        warn!(error = %e, "failed to renew WING OSC subscription, stopping");
                        return;
                    }
                }
                Err(e) => warn!(error = ?e, "failed to encode WING subscription request"),
            }
            // Subscriptions expire after 10s per the spec; renew with margin.
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(8)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_path_covers_lcl_1_to_8_only() {
        assert_eq!(WingAdapter::channel_path(1, "g").unwrap(), "/io/in/LCL/1/g");
        assert_eq!(WingAdapter::channel_path(8, "vph").unwrap(), "/io/in/LCL/8/vph");
        assert!(WingAdapter::channel_path(0, "g").is_err());
        assert!(WingAdapter::channel_path(9, "g").is_err());
    }

    #[test]
    fn parse_headamp_addr_round_trips_known_paths() {
        assert!(matches!(
            parse_headamp_addr("/io/in/LCL/1/g"),
            Some((1, HeadampField::Gain))
        ));
        assert!(matches!(
            parse_headamp_addr("/io/in/LCL/8/vph"),
            Some((8, HeadampField::Phantom))
        ));
        assert!(parse_headamp_addr("/io/in/LCL/1/mute").is_none());
        assert!(parse_headamp_addr("/ch/1/fdr").is_none());
    }

    #[test]
    fn last_numeric_value_matches_spec_reply_shapes() {
        // "/ch/1/fdr~~~,sff~~~~-oo~[0.0000][-144.0000]" shape: string, raw
        // float, actual float - last one wins.
        let fdr_like = vec![
            OscType::String("-oo".into()),
            OscType::Float(0.0),
            OscType::Float(-144.0),
        ];
        assert_eq!(last_numeric_value(&fdr_like), Some(-144.0));

        // "/ch/1/mute~~,sfi~~~~1~~~[1.0000][1]" shape: string, float, int.
        let mute_like = vec![
            OscType::String("1".into()),
            OscType::Float(1.0),
            OscType::Int(1),
        ];
        assert_eq!(last_numeric_value(&mute_like), Some(1.0));
    }

    #[test]
    fn parse_info_reply_extracts_model_from_csv_string() {
        let msg = OscMessage {
            addr: "/?".to_string(),
            args: vec![OscType::String(
                "WING,192.168.1.71,PGM,ngc-full,NO_SERIAL,1.07.2-40-g1b1b292b:develop".into(),
            )],
        };
        let info = parse_info_reply(&msg, "10.0.0.5".parse().unwrap()).unwrap();
        assert_eq!(info.vendor, "Behringer");
        assert_eq!(info.model, "ngc-full");
    }

    #[tokio::test]
    async fn identify_resolves_from_question_mark_reply() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let mut adapter = WingAdapter::new("wing-1", mock_addr);
        adapter.connect().await.unwrap();

        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                let (len, from) = mock.recv_from(&mut buf).await.unwrap();
                let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
                if let OscPacket::Message(m) = &packet {
                    if m.addr == "/?" {
                        let reply = OscPacket::Message(OscMessage {
                            addr: "/?".to_string(),
                            args: vec![OscType::String("WING,10.0.0.5,PGM,ngc-full,NO_SERIAL,1.0".into())],
                        });
                        let bytes = rosc::encoder::encode(&reply).unwrap();
                        mock.send_to(&bytes, from).await.unwrap();
                        return;
                    }
                }
                // Otherwise it was the /*s subscription renewal - keep waiting.
            }
        });

        let info = adapter.identify().await.unwrap();
        assert_eq!(info.vendor, "Behringer");
        assert_eq!(info.model, "ngc-full");
        server.abort();
    }

    #[tokio::test]
    async fn disconnect_stops_the_receive_loop() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut adapter = WingAdapter::new("wing-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();
        let mut events = adapter.subscribe();

        // Learn the adapter's ephemeral port from its /*s subscription renewal.
        let mut buf = [0u8; 512];
        let (_, from) = mock.recv_from(&mut buf).await.unwrap();

        adapter.disconnect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await; // let the loop actually exit

        let packet = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: "/io/in/LCL/1/g".to_string(),
            args: vec![OscType::Float(10.0)],
        }))
        .unwrap();
        mock.send_to(&packet, from).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(result.is_err(), "must not receive events after disconnect");
    }
}

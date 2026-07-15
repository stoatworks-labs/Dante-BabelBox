//! `DeviceAdapter` for Yamaha DM3/DM3S, built from the official "DM3
//! Series OSC Specifications Version 1.0.0" (usa.yamaha.com). This is the
//! only Yamaha console in this project with a byte/field-level public
//! spec - CL/QL/DM7 and Rio/Tio HA control use the legacy AD8HR MIDI
//! protocol (per Yamaha's "Dante-MY16-AUD & R series HA Remote Control
//! Guide"), which is a setup/configuration guide, not a wire-format spec,
//! and remains unimplemented. Whether DM7 shares DM3's OSC dialect is
//! unconfirmed - don't assume it without checking DM7's own spec.
//!
//! Transport: plain OSC over UDP, port 49900. Message format:
//! `/yosc:req/<action>/<address>/<X>/<Y> <value>`, e.g.
//! `/yosc:req/set/MIXER:Current/InCh/Fader/Level/1/1 -32768`. `<Y>` is
//! always present as a literal `1` even for single-dimensional parameters
//! with no real Y axis (confirmed from the spec's own worked examples).
//!
//! Preamp parameters live under `IO:Current/InCh` (`Local Input Num` as
//! X, 1-16 on DM3/DM3S):
//!   - `HAGain`: integer 0..64, **directly in dB** (coarse whole-dB steps
//!     only - no fractional resolution, a real protocol limitation, not
//!     a bridge shortcoming).
//!   - `48VOn`: integer 0/1.
//!
//! GAPS not covered by the spec (flagged rather than guessed): only
//! `set` actions are shown in the parameter table; no `get` example
//! exists for any parameter (only for Scene, via `sscurrent_ex`), and no
//! subscribe/unsolicited-push mechanism is documented at all (unlike
//! X32's `/xremote` or WING's `/*s`). This adapter sends a best-effort
//! `get` action for `get_state` (by analogy with `set`) and otherwise
//! relies on whatever the console spontaneously sends, if anything -
//! unconfirmed behaviour, not a documented guarantee.

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

const HA_GAIN_ADDR: &str = "IO:Current/InCh/HAGain";
const PHANTOM_ADDR: &str = "IO:Current/InCh/48VOn";
const GAIN_MIN_DB: f32 = 0.0;
const GAIN_MAX_DB: f32 = 64.0;

type PendingIdentify = Arc<StdMutex<Option<oneshot::Sender<DeviceInfo>>>>;

pub struct Dm3Adapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HashMap<u16, PreampState>>>,
    pending_identify: PendingIdentify,
    cancel: CancellationToken,
}

impl Dm3Adapter {
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

    fn check_channel(channel: u16) -> AdapterResult<()> {
        // DM3/DM3S support Local Input 1-16; other DM-family models are
        // unconfirmed, so this only rejects obviously-invalid input
        // rather than enforcing a hard per-model ceiling.
        if channel == 0 || channel > 128 {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(())
    }

    async fn send_raw(&self, addr: String, args: Vec<OscType>) -> AdapterResult<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let packet = OscPacket::Message(OscMessage { addr, args });
        let bytes = rosc::encoder::encode(&packet)
            .map_err(|e| AdapterError::Protocol(format!("OSC encode failed: {e:?}")))?;
        socket
            .send(&bytes)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        Ok(())
    }

    async fn send(&self, action: &str, address: &str, channel: u16, value: OscType) -> AdapterResult<()> {
        self.send_raw(format!("/yosc:req/{action}/{address}/{channel}/1"), vec![value])
            .await
    }
}

#[async_trait]
impl DeviceAdapter for Dm3Adapter {
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

        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        // Weakest-signal probe of the four adapters: the spec documents
        // no identify/model query at all. This reuses the one documented
        // "get"-shaped request in the whole spec - "Get Current Scene_A
        // Number" (`sscurrent_ex`) - and treats any reply mentioning it as
        // DM3 presence. Model is reported as unconfirmed since nothing in
        // the spec distinguishes DM3 from DM3S over the wire.
        let (tx, rx) = oneshot::channel();
        *self.pending_identify.lock().unwrap() = Some(tx);
        self.send_raw(
            "/yosc:req/sscurrent_ex".to_string(),
            vec![OscType::String("scene_a".to_string())],
        )
        .await?;
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(|_| AdapterError::Protocol("identify: timed out waiting for a reply".into()))?
            .map_err(|_| AdapterError::Protocol("identify: reply channel dropped".into()))
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        Self::check_channel(channel)?;
        let clamped = gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i32;
        self.send("set", HA_GAIN_ADDR, channel, OscType::Int(clamped)).await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        Self::check_channel(channel)?;
        self.send("set", PHANTOM_ADDR, channel, OscType::Int(on as i32)).await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        Self::check_channel(channel)?;
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        // Best-effort per the module doc comment - "get" is not shown in
        // the spec for any parameter; this may simply not work.
        self.send("get", HA_GAIN_ADDR, channel, OscType::Int(0)).await?;
        self.send("get", PHANTOM_ADDR, channel, OscType::Int(0)).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!(
            "no reply for channel {channel} preamp state (DM3 'get' support and \
             spontaneous-update behaviour are both unconfirmed by the spec)"
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

/// Finds the `HAGain`/`48VOn` path segment anywhere in the address and
/// reads the channel number from the segment immediately after it -
/// robust to whichever `<action>` prefix (`set`, `get`, or an unknown
/// reply-side action) the console actually uses.
fn parse_headamp_addr(addr: &str) -> Option<(u16, HeadampField)> {
    let parts: Vec<&str> = addr.split('/').collect();
    let idx = parts.iter().position(|&p| p == "HAGain" || p == "48VOn")?;
    let field = if parts[idx] == "HAGain" {
        HeadampField::Gain
    } else {
        HeadampField::Phantom
    };
    let channel: u16 = parts.get(idx + 1)?.parse().ok()?;
    Some((channel, field))
}

fn last_int_value(args: &[OscType]) -> Option<i32> {
    args.iter().rev().find_map(|a| match a {
        OscType::Int(i) => Some(*i),
        OscType::Float(f) => Some(*f as i32),
        _ => None,
    })
}

/// Recognizes a reply to the `sscurrent_ex "scene_a"` identify probe -
/// either by address (if the console echoes `sscurrent_ex` back) or by a
/// `"scene_a"` string argument. Deliberately loose since the spec gives no
/// worked reply example for this at all; see the module doc comment.
fn parse_identify_reply(msg: &OscMessage, address: IpAddr) -> Option<DeviceInfo> {
    let addr_matches = msg.addr.contains("sscurrent_ex");
    let arg_matches = msg
        .args
        .iter()
        .any(|a| matches!(a, OscType::String(s) if s == "scene_a"));
    if !addr_matches && !arg_matches {
        return None;
    }
    Some(DeviceInfo {
        vendor: "Yamaha".to_string(),
        model: "DM3 series (unconfirmed sub-model)".to_string(),
        address,
    })
}

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
                        warn!(device = %id, error = %e, "DM3 OSC socket read failed, stopping receive loop");
                        return;
                    }
                },
            };
            match rosc::decoder::decode_udp(&buf[..len]) {
                Ok((_, OscPacket::Message(msg))) => {
                    if let Some(info) = parse_identify_reply(&msg, remote_ip) {
                        if let Some(sender) = pending_identify.lock().unwrap().take() {
                            let _ = sender.send(info);
                        }
                        continue;
                    }
                    handle_message(msg, &id, &tx, &state).await
                }
                Ok((_, OscPacket::Bundle(_))) => {
                    debug!(device = %id, "ignoring OSC bundle from DM3");
                }
                Err(e) => debug!(device = %id, error = ?e, "dropping malformed OSC packet"),
            }
        }
    });
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
    let Some(value) = last_int_value(&msg.args) else {
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
            HeadampField::Gain => entry.gain_db = value as f32,
            HeadampField::Phantom => entry.phantom = value != 0,
        }
        *entry
    };

    debug!(device = %id, channel, ?field, value, "DM3 headamp update");
    let _ = tx.send(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: new_state,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headamp_addr_finds_channel_regardless_of_action_prefix() {
        assert!(matches!(
            parse_headamp_addr("/yosc:req/set/IO:Current/InCh/HAGain/3/1"),
            Some((3, HeadampField::Gain))
        ));
        assert!(matches!(
            parse_headamp_addr("/yosc:rpl/get/IO:Current/InCh/48VOn/16/1"),
            Some((16, HeadampField::Phantom))
        ));
        assert!(parse_headamp_addr("/yosc:req/set/MIXER:Current/InCh/Fader/Level/1/1").is_none());
    }

    #[test]
    fn gain_is_clamped_to_documented_0_to_64_range() {
        assert_eq!(0.0f32.clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i32, 0);
        assert_eq!(100.0f32.clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i32, 64);
        assert_eq!((-5.0f32).clamp(GAIN_MIN_DB, GAIN_MAX_DB).round() as i32, 0);
    }

    #[tokio::test]
    async fn set_gain_sends_expected_osc_message() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();

        let mut adapter = Dm3Adapter::new("dm3-1", listener_addr);
        adapter.connect().await.unwrap();
        adapter.set_gain(3, 42.0).await.unwrap();

        let mut buf = [0u8; 512];
        let (len, _) = tokio::time::timeout(Duration::from_secs(1), listener.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
        let OscPacket::Message(m) = packet else {
            panic!("expected message")
        };
        assert_eq!(m.addr, "/yosc:req/set/IO:Current/InCh/HAGain/3/1");
        assert_eq!(m.args, vec![OscType::Int(42)]);
    }

    #[test]
    fn parse_identify_reply_matches_by_address_or_arg() {
        let addr: IpAddr = "10.0.0.5".parse().unwrap();

        let by_addr = OscMessage {
            addr: "/yosc:rpl/sscurrent_ex".to_string(),
            args: vec![OscType::String("1".into())],
        };
        assert!(parse_identify_reply(&by_addr, addr).is_some());

        let by_arg = OscMessage {
            addr: "/yosc:req/sscurrent_ex".to_string(),
            args: vec![OscType::String("scene_a".into()), OscType::String("1".into())],
        };
        assert!(parse_identify_reply(&by_arg, addr).is_some());

        let unrelated = OscMessage {
            addr: "/yosc:req/set/MIXER:Current/InCh/Fader/Level/1/1".to_string(),
            args: vec![OscType::Int(-32768)],
        };
        assert!(parse_identify_reply(&unrelated, addr).is_none());
    }

    #[tokio::test]
    async fn identify_resolves_from_scene_reply() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();

        let mut adapter = Dm3Adapter::new("dm3-1", listener_addr);
        adapter.connect().await.unwrap();

        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (len, from) = listener.recv_from(&mut buf).await.unwrap();
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
            assert!(matches!(&packet, OscPacket::Message(m) if m.addr == "/yosc:req/sscurrent_ex"));

            let reply = OscPacket::Message(OscMessage {
                addr: "/yosc:req/sscurrent_ex".to_string(),
                args: vec![OscType::String("scene_a".into()), OscType::String("3".into())],
            });
            let bytes = rosc::encoder::encode(&reply).unwrap();
            listener.send_to(&bytes, from).await.unwrap();
        });

        let info = adapter.identify().await.unwrap();
        assert_eq!(info.vendor, "Yamaha");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_stops_the_receive_loop() {
        let mock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut adapter = Dm3Adapter::new("dm3-1", mock.local_addr().unwrap());
        adapter.connect().await.unwrap();
        let mut events = adapter.subscribe();

        // Learn the adapter's ephemeral port from a fire-and-forget set.
        adapter.set_gain(1, 10.0).await.unwrap();
        let mut buf = [0u8; 512];
        let (_, from) = mock.recv_from(&mut buf).await.unwrap();

        adapter.disconnect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await; // let the loop actually exit

        let packet = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: "/yosc:req/set/IO:Current/InCh/HAGain/1/1".to_string(),
            args: vec![OscType::Int(20)],
        }))
        .unwrap();
        mock.send_to(&packet, from).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
        assert!(result.is_err(), "must not receive events after disconnect");
    }
}

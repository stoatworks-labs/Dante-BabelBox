//! `DeviceAdapter` for Yamaha R-series head amps (Rio/Tio) and QL/CL
//! consoles, speaking the `MBC` block over Audinate ConMon.
//!
//! Built against `docs/yamaha-ha-remote-over-dante.md`, which was captured
//! from a real QL1 + Rio3224-D2 and **proven by writing gain to the real
//! stagebox**. The codec in [`codec`] is byte-verified against the exact
//! frame the Rio accepted. This adapter has still never been run against
//! hardware - see "Status" below for the precise line.
//!
//! ## How this differs from every other adapter here
//!
//! Head-amp control is not a request/response protocol with per-parameter
//! addressing. The console **broadcasts the entire 32-channel array** every
//! time anything changes, and that whole-array form is the only one ever
//! observed - and the only one proven to work.
//!
//! Two consequences, both deliberate:
//!
//! - [`set_gain`](DeviceAdapter::set_gain) has to send all 32 channels, so
//!   it needs to know the other 31. It refuses to write until it has heard
//!   the device's own broadcast, rather than inventing a default and
//!   flattening channels the user never touched.
//! - There is no unicast conversation to have. The adapter joins the
//!   ConMon multicast group and both reads and writes there.
//!
//! ## Identity, and why writing means impersonation
//!
//! MBC addresses devices by their **Yamaha MAC**, which is a different NIC
//! from the Dante MAC, and the ConMon envelope carries the sender's
//! EUI-64 and message class. Writing gain therefore means presenting the
//! console's identity. On a network where the real console is also
//! connected, both will be writing the same parameters - and per §7 of the
//! spec, on any resync **the console's stored scene wins**. That is a
//! property of the protocol, not something this adapter can arbitrate.
//!
//! ## Status
//!
//! - The wire format is **verified on hardware** (a real Rio3224-D2 acted
//!   on a frame this codec reproduces byte for byte).
//! - This adapter is **not** verified on hardware. It is tested against
//!   captured frames and a loopback socket, like every other adapter in
//!   this workspace.
//! - `pad` is reported as `None`: the spec's §6 arrays that would carry it
//!   have known element widths but unknown meaning, and guessing which one
//!   is pad would be fabrication.

pub mod codec;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use async_trait::async_trait;
use dante_babelbox_core::{
    ChangedFields, AdapterError, AdapterResult, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent, PreampState,
};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use codec::{
    blocks_in_conmon_payload, gain_broadcast, phantom_broadcast, wrap_conmon, MbcBlock,
    CONMON_STATUS_GROUP, CONMON_STATUS_PORT, GAIN_MAX_DB, GAIN_MIN_DB, HEADAMP_CHANNELS,
};

/// ConMon message class the QL1 sends under. Writing as the console means
/// using this together with the console's EUI-64 and Yamaha MAC.
pub const MESSAGE_CLASS_CONSOLE: u32 = 0x072e1002;
/// Message class observed from the Rio3224-D2.
pub const MESSAGE_CLASS_DEVICE: u32 = 0x07311002;

/// The identity this adapter presents when it writes, and the device it
/// writes to.
///
/// There is no discovery shortcut for these: the Rio answers nothing on
/// Audinate's control ports (4440/4444/4455/8800), so its Yamaha MAC has
/// to come from mDNS plus a ConMon observation, or from configuration.
#[derive(Debug, Clone)]
pub struct MbcIdentity {
    /// Yamaha MAC to send as - the console's, when impersonating it.
    pub src_mac: [u8; 6],
    /// Yamaha MAC of the head-amp device being controlled.
    pub dst_mac: [u8; 6],
    /// EUI-64 in the ConMon envelope. Note the two devices derive this
    /// differently: the QL1 pads its MAC with zeroes
    /// (`00:1d:c1:17:ea:2c` -> `001dc117ea2c0000`) while the Rio uses the
    /// standard `fffe` insertion (`001dc1fffe25df04`). Copy what the
    /// device you are impersonating actually sends.
    pub sender_eui64: [u8; 8],
    pub message_class: u32,
}

impl MbcIdentity {
    /// Identity of the QL1 in the reference capture. Useful for tests and
    /// for reproducing the verified write; real deployments must supply
    /// their own MACs.
    pub fn reference_ql1(dst_mac: [u8; 6]) -> Self {
        Self {
            src_mac: [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6],
            dst_mac,
            sender_eui64: [0x00, 0x1d, 0xc1, 0x17, 0xea, 0x2c, 0x00, 0x00],
            message_class: MESSAGE_CLASS_CONSOLE,
        }
    }
}

/// Head-amp state as last heard from the device.
#[derive(Debug, Clone, Default)]
struct HeadampState {
    gain_db: Option<[f32; HEADAMP_CHANNELS]>,
    phantom: Option<[bool; HEADAMP_CHANNELS]>,
    /// Raw metering bytes. Deliberately uncalibrated - see the codec.
    metering: Option<[u8; HEADAMP_CHANNELS]>,
}

pub struct MbcAdapter {
    id: Arc<str>,
    identity: MbcIdentity,
    /// Local interface to send from and join the multicast group on.
    interface: Ipv4Addr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HeadampState>>,
    sequence: Arc<Mutex<u16>>,
    cancel: CancellationToken,
}

impl MbcAdapter {
    pub fn new(id: impl Into<Arc<str>>, identity: MbcIdentity, interface: Ipv4Addr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            identity,
            interface,
            socket: None,
            tx,
            state: Arc::new(Mutex::new(HeadampState::default())),
            sequence: Arc::new(Mutex::new(0)),
            cancel: CancellationToken::new(),
        }
    }

    /// Channels are 1-based on the surface; the wire arrays are 0-based.
    fn channel_index(channel: u16) -> AdapterResult<usize> {
        let idx = channel
            .checked_sub(1)
            .filter(|&i| (i as usize) < HEADAMP_CHANNELS)
            .ok_or(AdapterError::UnsupportedChannel(channel))?;
        Ok(idx as usize)
    }

    async fn send_block(&self, block: MbcBlock) -> AdapterResult<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;

        let sequence = {
            let mut seq = self.sequence.lock().await;
            *seq = seq.wrapping_add(1);
            *seq
        };
        let packet = wrap_conmon(
            &block.encode(),
            sequence,
            self.identity.sender_eui64,
            self.identity.message_class,
        );
        let group = SocketAddrV4::new(CONMON_STATUS_GROUP.into(), CONMON_STATUS_PORT);
        socket
            .send_to(&packet, SocketAddr::V4(group))
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        Ok(())
    }

    /// The device's whole gain array, or an error explaining why writing
    /// is not safe yet.
    async fn require_gains(&self) -> AdapterResult<[f32; HEADAMP_CHANNELS]> {
        self.state.lock().await.gain_db.ok_or_else(|| {
            AdapterError::Protocol(
                "no head-amp broadcast heard yet; a gain write sends all 32 channels at once, \
                 so writing now would overwrite channels with invented values"
                    .into(),
            )
        })
    }

    async fn require_phantom(&self) -> AdapterResult<[bool; HEADAMP_CHANNELS]> {
        self.state.lock().await.phantom.ok_or_else(|| {
            AdapterError::Protocol(
                "no phantom broadcast heard yet; a phantom write sends all 32 channels at once, \
                 so writing now would overwrite channels with invented values"
                    .into(),
            )
        })
    }
}

#[async_trait]
impl DeviceAdapter for MbcAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let socket = bind_conmon_socket(self.interface)?;
        socket
            .join_multicast_v4(CONMON_STATUS_GROUP.into(), self.interface)
            .map_err(|e| {
                AdapterError::Connection(format!("joining ConMon multicast group: {e}"))
            })?;
        let socket = Arc::new(socket);
        self.socket = Some(Arc::clone(&socket));

        spawn_receive_loop(
            Arc::clone(&socket),
            Arc::clone(&self.id),
            self.identity.dst_mac,
            self.tx.clone(),
            Arc::clone(&self.state),
            self.cancel.clone(),
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.cancel.cancel();
        self.socket = None;
        Ok(())
    }

    /// Identification is **passive**. Per §9 of the spec the Rio3224-D2
    /// answers nothing on any Audinate control port, so there is no query
    /// to send; the only proof a device is present is hearing it speak.
    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        if self.socket.is_none() {
            return Err(AdapterError::Connection("not connected".into()));
        }
        Ok(DeviceInfo {
            vendor: "Yamaha".to_string(),
            model: "R-series head amp (model not carried on the wire)".to_string(),
            address: IpAddr::V4(self.interface),
        })
    }

    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        let idx = Self::channel_index(channel)?;
        let mut gains = self.require_gains().await?;
        gains[idx] = gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        self.send_block(gain_broadcast(
            self.identity.src_mac,
            self.identity.dst_mac,
            &gains,
        ))
        .await
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        let idx = Self::channel_index(channel)?;
        let mut phantom = self.require_phantom().await?;
        phantom[idx] = on;
        self.send_block(phantom_broadcast(
            self.identity.src_mac,
            self.identity.dst_mac,
            &phantom,
        ))
        .await
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        let idx = Self::channel_index(channel)?;
        let state = self.state.lock().await;
        let gain_db = state.gain_db.ok_or_else(|| {
            AdapterError::Protocol("no head-amp broadcast heard yet for this device".into())
        })?[idx];
        Ok(PreampState {
            gain_db,
            phantom: state.phantom.map(|p| p[idx]).unwrap_or(false),
            // Which of the 0x0722 arrays carries pad is unresolved - see
            // the module doc comment.
            pad: None,
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

/// Binds the ConMon status port with address reuse, so this can coexist
/// with Dante Controller and anything else listening on the same group.
fn bind_conmon_socket(interface: Ipv4Addr) -> AdapterResult<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .set_reuse_address(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    #[cfg(unix)]
    socket
        .set_reuse_port(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, CONMON_STATUS_PORT)).into())
        .map_err(|e| {
            AdapterError::Connection(format!("binding ConMon port {CONMON_STATUS_PORT}: {e}"))
        })?;
    // TTL 1: ConMon is link-local and must not be routed off the segment.
    socket
        .set_multicast_ttl_v4(1)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .set_multicast_if_v4(&interface)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;

    UdpSocket::from_std(socket.into()).map_err(|e| AdapterError::Connection(e.to_string()))
}

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    device_mac: [u8; 6],
    tx: broadcast::Sender<PreampEvent>,
    state: Arc<Mutex<HeadampState>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let len = tokio::select! {
                _ = cancel.cancelled() => return,
                result = socket.recv(&mut buf) => match result {
                    Ok(len) => len,
                    Err(e) => {
                        warn!(device = %id, error = %e, "ConMon socket read failed, stopping receive loop");
                        return;
                    }
                },
            };
            for block in blocks_in_conmon_payload(&buf[..len]) {
                // Accept both what the device says and what the console
                // tells it: the console's broadcast is the authoritative
                // state after a resync (spec §7).
                if block.src_mac != device_mac && block.dst_mac != device_mac {
                    continue;
                }
                apply_block(&block, &id, &tx, &state).await;
            }
        }
    });
}

async fn apply_block(
    block: &MbcBlock,
    id: &Arc<str>,
    tx: &broadcast::Sender<PreampEvent>,
    state: &Arc<Mutex<HeadampState>>,
) {
    let mut changed: Vec<usize> = Vec::new();
    {
        let mut guard = state.lock().await;

        if let Some(gains) = block.gain_db() {
            if let Ok(array) = <[f32; HEADAMP_CHANNELS]>::try_from(gains.as_slice()) {
                changed = match guard.gain_db {
                    Some(previous) => (0..HEADAMP_CHANNELS)
                        .filter(|&i| previous[i] != array[i])
                        .collect(),
                    None => (0..HEADAMP_CHANNELS).collect(),
                };
                guard.gain_db = Some(array);
            }
        } else if let Some(phantom) = block.phantom() {
            if let Ok(array) = <[bool; HEADAMP_CHANNELS]>::try_from(phantom.as_slice()) {
                changed = match guard.phantom {
                    Some(previous) => (0..HEADAMP_CHANNELS)
                        .filter(|&i| previous[i] != array[i])
                        .collect(),
                    None => (0..HEADAMP_CHANNELS).collect(),
                };
                guard.phantom = Some(array);
            }
        } else if let Some(metering) = block.metering_raw() {
            if let Ok(array) = <[u8; HEADAMP_CHANNELS]>::try_from(metering) {
                guard.metering = Some(array);
            }
            return; // metering is not preamp state; no events
        } else {
            debug!(
                device = %id,
                opcode = format!("{:#06x}", block.opcode),
                subop = format!("{:#04x}", block.subop),
                elements = block.count,
                "unhandled MBC block"
            );
            return;
        }
    }

    // Emit outside the lock. A pairing resync rewrites all 32 at once
    // (spec §7); that is one device event per channel, not 32 user edits,
    // and the Router's echo suppression is what disambiguates.
    let snapshot = state.lock().await.clone();
    for idx in changed {
        let Some(gains) = snapshot.gain_db else {
            continue;
        };
        // `phantom` falls back to false when the snapshot has never carried
        // it — that is a placeholder, not a reading, so say so rather than
        // relaying it and switching 48 V off on the mapped peer.
        let changed = ChangedFields {
            gain: true,
            phantom: snapshot.phantom.is_some(),
            pad: false,
        };
        let _ = tx.send(PreampEvent {
            address: PreampAddress::new(id.to_string(), idx as u16 + 1),
            state: PreampState {
                gain_db: gains[idx],
                phantom: snapshot.phantom.map(|p| p[idx]).unwrap_or(false),
                pad: None,
            },
            changed,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIO_MAC: [u8; 6] = [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04];

    fn adapter() -> MbcAdapter {
        MbcAdapter::new(
            "rio-1",
            MbcIdentity::reference_ql1(RIO_MAC),
            Ipv4Addr::LOCALHOST,
        )
    }

    #[test]
    fn channels_are_one_based_and_bounded_at_32() {
        assert_eq!(MbcAdapter::channel_index(1).unwrap(), 0);
        assert_eq!(MbcAdapter::channel_index(32).unwrap(), 31);
        assert!(MbcAdapter::channel_index(0).is_err());
        assert!(MbcAdapter::channel_index(33).is_err());
    }

    #[tokio::test]
    async fn refuses_to_write_gain_before_hearing_the_device() {
        let mut a = adapter();
        let err = a.set_gain(1, 10.0).await.unwrap_err();
        let AdapterError::Protocol(message) = err else {
            panic!("expected a protocol error explaining the refusal");
        };
        assert!(
            message.contains("all 32 channels"),
            "the error must explain why a blind write is unsafe, got: {message}"
        );
    }

    #[tokio::test]
    async fn a_gain_broadcast_updates_state_and_emits_per_channel_events() {
        let a = adapter();
        let mut events = a.subscribe();

        let mut gains = [-6.0f32; HEADAMP_CHANNELS];
        gains[4] = 25.0;
        let block = gain_broadcast(RIO_MAC, [0xff; 6], &gains);
        apply_block(&block, &a.id, &a.tx, &a.state).await;

        assert_eq!(a.state.lock().await.gain_db.unwrap()[4], 25.0);
        let first = events.recv().await.unwrap();
        assert_eq!(first.address.channel, 1);
        assert_eq!(first.state.gain_db, -6.0);
    }

    #[tokio::test]
    async fn only_changed_channels_are_reported_after_the_first_broadcast() {
        let a = adapter();

        let gains = [-6.0f32; HEADAMP_CHANNELS];
        apply_block(
            &gain_broadcast(RIO_MAC, [0xff; 6], &gains),
            &a.id,
            &a.tx,
            &a.state,
        )
        .await;

        let mut events = a.subscribe(); // subscribe after the initial sync
        let mut moved = gains;
        moved[7] = 12.0;
        apply_block(
            &gain_broadcast(RIO_MAC, [0xff; 6], &moved),
            &a.id,
            &a.tx,
            &a.state,
        )
        .await;

        let event = events.recv().await.unwrap();
        assert_eq!(event.address.channel, 8, "only input 8 moved");
        assert_eq!(event.state.gain_db, 12.0);
        assert!(
            events.try_recv().is_err(),
            "no other channel should be reported"
        );
    }

    #[tokio::test]
    async fn metering_updates_state_but_never_emits_preamp_events() {
        let a = adapter();
        let mut events = a.subscribe();

        let block = MbcBlock {
            version: 1,
            src_mac: RIO_MAC,
            dst_mac: [0xff; 6],
            flags: 0x13,
            opcode: codec::OPCODE_METERING,
            subop: codec::SUBOP_METERING,
            count: 32,
            start_index: 0,
            data: vec![48u8; 32],
        };
        apply_block(&block, &a.id, &a.tx, &a.state).await;

        assert_eq!(a.state.lock().await.metering.unwrap()[0], 48);
        assert!(
            events.try_recv().is_err(),
            "metering is not preamp state and must not surface as a gain event"
        );
    }

    #[tokio::test]
    async fn get_state_reports_gain_and_phantom_but_never_invents_pad() {
        let mut a = adapter();
        let mut gains = [-6.0f32; HEADAMP_CHANNELS];
        gains[0] = 18.0;
        apply_block(
            &gain_broadcast(RIO_MAC, [0xff; 6], &gains),
            &a.id,
            &a.tx,
            &a.state,
        )
        .await;

        let mut phantom = [false; HEADAMP_CHANNELS];
        phantom[0] = true;
        apply_block(
            &phantom_broadcast(RIO_MAC, [0xff; 6], &phantom),
            &a.id,
            &a.tx,
            &a.state,
        )
        .await;

        let state = a.get_state(1).await.unwrap();
        assert_eq!(state.gain_db, 18.0);
        assert!(state.phantom);
        assert_eq!(
            state.pad, None,
            "pad must stay None until its subop is identified"
        );
    }

    #[tokio::test]
    async fn writes_preserve_every_channel_the_caller_did_not_touch() {
        let a = adapter();
        let mut gains = [-6.0f32; HEADAMP_CHANNELS];
        gains[10] = 30.0;
        apply_block(
            &gain_broadcast(RIO_MAC, [0xff; 6], &gains),
            &a.id,
            &a.tx,
            &a.state,
        )
        .await;

        let mut updated = a.require_gains().await.unwrap();
        updated[0] = 5.0;
        let block = gain_broadcast(a.identity.src_mac, a.identity.dst_mac, &updated);

        let decoded = &blocks_in_conmon_payload(&wrap_conmon(
            &block.encode(),
            1,
            a.identity.sender_eui64,
            a.identity.message_class,
        ))[0];
        let out = decoded.gain_db().unwrap();
        assert_eq!(out[0], 5.0, "the requested change is applied");
        assert_eq!(
            out[10], 30.0,
            "an untouched channel keeps the device's value"
        );
    }
}

//! `DeviceAdapter` for an **emulated** Yamaha R-series stagebox: a Rio that
//! exists only on the network, so that a Yamaha console's own head-amp
//! controls can be sent on to hardware it does not know.
//!
//! Every other adapter here dials a device that is there. This one *is* the
//! device. A CL or QL console sees a Rio, mounts it, and drives its gain
//! and +48 V the way it drives any Rio; each move comes out of
//! [`subscribe`](DeviceAdapter::subscribe) as a [`PreampEvent`], and a
//! `[[mapping]]` carries it to whatever really holds the signal: another
//! maker's stagebox, a radio-mic receiver's output level, an audio
//! interface's mixer. In the other direction,
//! [`set_gain`](DeviceAdapter::set_gain) changes what the emulated Rio
//! holds and announces it, as a Rio announces a change made at its own end.
//!
//! Two layers do the work, and neither is in this file:
//!
//! - [`dante_babelbox_emulation`] puts a Dante device on the network: mDNS,
//!   ARC, ConMon. It knows nothing about Yamaha.
//! - [`super::device::HeadampDevice`] answers the console's `MBC` messages.
//!   It knows nothing about sockets.
//!
//! ## What a console does to it
//!
//! **On pairing the console writes its own gain and +48 V to all 32
//! channels** (spec §7: the console's stored state wins). Those writes are
//! reported like any others, so everything mapped from this device receives
//! the console's scene values the moment the console finds it. That is what
//! a real Rio gets too, and it is the point of the exercise, but it means a
//! mapped preamp's gain and phantom power follow the console from that
//! moment. Map with that in mind.
//!
//! ## No audio
//!
//! The emulated Rio transmits nothing. A console that patches one of its
//! inputs from the emulated Rio subscribes to a channel that never
//! connects. Getting the real signal onto the same console channel is a
//! patching question with more than one answer, none of them tested; see
//! `docs/dante-emulation.md`.
//!
//! ## Status
//!
//! Tested against a capture, not a console. Every answer the head-amp layer
//! gives is, byte for byte, the answer a real Rio3224-D2 gave a real QL1
//! (`examples/replay_mbc_device.rs`: 237 of 237), and the Dante layer
//! matches the same Rio wherever audio is not involved. **No console has
//! been put in front of it.**

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dante_babelbox_core::{
    AdapterError, AdapterResult, ChangedFields, DeviceAdapter, DeviceInfo, PreampAddress, PreampEvent,
    PreampState,
};
use dante_babelbox_emulation::{
    local_mac, DeviceIdentity, Persona, Ports, VendorChannel, VirtualDevice, VirtualDeviceConfig,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::codec::{blocks_in_conmon_payload, wrap_conmon_blocks, HEADAMP_CHANNELS};
use super::device::{Change, HeadampDevice, HeadampIdentity, Outgoing};
use super::MESSAGE_CLASS_DEVICE;

/// The Rio streamed metering at about 31 frames a second.
const METER_PERIOD: Duration = Duration::from_millis(32);
/// The console asked for metering again every 9.6 s or so. A stream nobody
/// has asked for in this long is stopped.
const METER_HOLD: Duration = Duration::from_secs(15);
/// Gap between the packets of the power-up announcement; the Rio's were a
/// few milliseconds apart.
const ANNOUNCE_GAP: Duration = Duration::from_millis(5);

/// The R-series model to present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RSeriesModel {
    /// The name the console knows the model by. It appears in the Dante
    /// device name (`Y004-Yamaha-Rio3224-D2-25df04`) and is what the device
    /// answers when asked its model.
    pub name: String,
    pub inputs: u16,
    pub outputs: u16,
    /// Audinate's id for the product (mDNS `model=`).
    pub audinate_model_id: String,
}

impl RSeriesModel {
    /// The one model every value here was captured from.
    pub fn rio3224_d2() -> Self {
        Self { name: "Rio3224-D2".into(), inputs: 32, outputs: 24, audinate_model_id: "8R19".into() }
    }
}

#[derive(Debug, Clone)]
pub struct EmulatedRioConfig {
    /// Local interface to present the device on: the Dante network's.
    pub interface: Ipv4Addr,
    /// The stagebox unit id, the `###` of `Y###`. A console mounts a device
    /// by this number, so it must not clash with a real unit.
    pub unit_id: u16,
    pub model: RSeriesModel,
    /// Dante device name. Left out, it is built the way Yamaha builds it:
    /// `Y<unit id>-Yamaha-<model>-<last three MAC bytes>`.
    pub name: Option<String>,
    /// The MAC the device claims. Left out, a locally administered one is
    /// derived from the device id and interface, the same on every run.
    pub mac: Option<[u8; 6]>,
    /// Answer as a Dante device (ARC, ConMon). Off, only the head-amp
    /// protocol runs.
    pub presence: bool,
    /// Advertise over mDNS.
    pub advertise: bool,
    /// Stream (silent) input metering while a console asks for it.
    pub meters: bool,
    pub ports: Ports,
}

impl EmulatedRioConfig {
    pub fn new(interface: Ipv4Addr, unit_id: u16) -> Self {
        Self {
            interface,
            unit_id,
            model: RSeriesModel::rio3224_d2(),
            name: None,
            mac: None,
            presence: true,
            advertise: true,
            meters: true,
            ports: Ports::default(),
        }
    }

    /// The identity this configuration presents for bridge device `id`.
    pub fn identity(&self, id: &str) -> DeviceIdentity {
        let mac = self.mac.unwrap_or_else(|| local_mac(&format!("{id}@{}", self.interface)));
        let name = self.name.clone().unwrap_or_else(|| {
            format!(
                "Y{:03X}-Yamaha-{}-{:02x}{:02x}{:02x}",
                self.unit_id, self.model.name, mac[3], mac[4], mac[5]
            )
        });
        DeviceIdentity { name, mac, ip: self.interface }
    }

    fn persona(&self) -> Persona {
        Persona {
            model_id: self.model.audinate_model_id.clone(),
            model_name: self.model.name.clone(),
            tx_channels: self.model.inputs,
            rx_channels: self.model.outputs,
            ..Persona::yamaha_rio3224_d2()
        }
    }
}

struct Running {
    /// Held for its lifetime: dropping it takes the device off the network.
    _device: VirtualDevice,
    vendor: VendorChannel,
    cancel: CancellationToken,
}

pub struct EmulatedRioAdapter {
    id: Arc<str>,
    config: EmulatedRioConfig,
    headamp: Arc<Mutex<HeadampDevice>>,
    tx: broadcast::Sender<PreampEvent>,
    running: Option<Running>,
}

impl EmulatedRioAdapter {
    pub fn new(id: impl Into<Arc<str>>, config: EmulatedRioConfig) -> Self {
        let id: Arc<str> = id.into();
        let identity = config.identity(&id);
        let headamp = HeadampDevice::new(HeadampIdentity {
            mac: identity.mac,
            unit_id: config.unit_id,
            model: config.model.name.clone(),
            dante_name: identity.name,
            inputs: config.model.inputs,
            outputs: config.model.outputs,
        });
        let (tx, _) = broadcast::channel(256);
        Self { id, config, headamp: Arc::new(Mutex::new(headamp)), tx, running: None }
    }

    /// Head-amp channels a mapping can use.
    pub fn channels(&self) -> u16 {
        self.config.model.inputs.min(HEADAMP_CHANNELS as u16)
    }

    /// The Dante identity being presented.
    pub fn identity(&self) -> DeviceIdentity {
        self.config.identity(&self.id)
    }

    fn check_channel(&self, channel: u16) -> AdapterResult<()> {
        if channel == 0 || channel > self.channels() {
            return Err(AdapterError::UnsupportedChannel(channel));
        }
        Ok(())
    }

    fn vendor(&self) -> AdapterResult<&VendorChannel> {
        self.running
            .as_ref()
            .map(|r| &r.vendor)
            .ok_or_else(|| AdapterError::Connection("the emulated device is not running".into()))
    }
}

/// Sends blocks as one ConMon packet from the device.
async fn send_blocks(vendor: &VendorChannel, blocks: &[Outgoing]) {
    let encoded: Vec<Vec<u8>> = blocks.iter().map(Outgoing::encode).collect();
    let refs: Vec<&[u8]> = encoded.iter().map(Vec::as_slice).collect();
    let packet = wrap_conmon_blocks(&refs, vendor.next_sequence(), vendor.device_id(), MESSAGE_CLASS_DEVICE);
    if let Err(e) = vendor.send(&packet).await {
        debug!(error = %e, "sending on the vendor channel failed");
    }
}

fn event(id: &str, headamp: &HeadampDevice, change: Change) -> Option<PreampEvent> {
    let (channel, changed) = match change {
        Change::Gain { channel, .. } => (channel, ChangedFields::GAIN),
        Change::Phantom { channel, .. } => (channel, ChangedFields::PHANTOM),
    };
    if channel > headamp.identity().inputs {
        return None; // the array is 32 wide whatever the model has
    }
    Some(PreampEvent {
        address: PreampAddress::new(id.to_string(), channel),
        state: PreampState { gain_db: headamp.gain_db(channel)?, phantom: headamp.phantom(channel)?, pad: None },
        changed,
    })
}

async fn run(
    id: Arc<str>,
    vendor: VendorChannel,
    headamp: Arc<Mutex<HeadampDevice>>,
    tx: broadcast::Sender<PreampEvent>,
    meters: bool,
    cancel: CancellationToken,
) {
    // Subscribe before announcing, so a console that answers at once is heard.
    let mut incoming = vendor.subscribe();

    let announcement = headamp.lock().expect("head-amp state").power_up_announcement();
    for packet in &announcement {
        send_blocks(&vendor, packet).await;
        tokio::time::sleep(ANNOUNCE_GAP).await;
    }

    let mut meter_until: Option<Instant> = None;
    let mut meter_tick = tokio::time::interval(METER_PERIOD);
    meter_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            received = incoming.recv() => {
                let datagram = match received {
                    Ok(datagram) => datagram,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                for block in blocks_in_conmon_payload(&datagram.payload) {
                    let (outcome, events) = {
                        let mut device = headamp.lock().expect("head-amp state");
                        let outcome = device.handle(&block);
                        let events: Vec<PreampEvent> =
                            outcome.changes.iter().filter_map(|c| event(&id, &device, *c)).collect();
                        (outcome, events)
                    };
                    // One packet per answer, which is how the Rio answered
                    // even a packet that asked two things.
                    for reply in &outcome.replies {
                        send_blocks(&vendor, std::slice::from_ref(reply)).await;
                    }
                    if outcome.metering_requested && meters {
                        if meter_until.is_none() {
                            info!(device = %id, "a console asked for metering");
                        }
                        meter_until = Some(Instant::now() + METER_HOLD);
                    }
                    for event in events {
                        let _ = tx.send(event);
                    }
                }
            }
            _ = meter_tick.tick(), if meter_until.is_some() => {
                if meter_until.is_some_and(|until| Instant::now() >= until) {
                    meter_until = None;
                    continue;
                }
                let frame = headamp.lock().expect("head-amp state").meter_frame();
                send_blocks(&vendor, std::slice::from_ref(&frame)).await;
            }
        }
    }
}

#[async_trait]
impl DeviceAdapter for EmulatedRioAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    /// Puts the device on the network. "Connect" is the trait's word; for
    /// this adapter it means "start existing".
    async fn connect(&mut self) -> AdapterResult<()> {
        if self.running.is_some() {
            return Ok(());
        }
        let identity = self.identity();
        let device = VirtualDevice::start(VirtualDeviceConfig {
            identity: identity.clone(),
            persona: self.config.persona(),
            presence: self.config.presence,
            advertise: self.config.presence && self.config.advertise,
            ports: self.config.ports,
        })
        .await
        .map_err(|e| AdapterError::Connection(format!("starting emulated device '{}': {e}", identity.name)))?;

        let vendor = device.vendor();
        let cancel = CancellationToken::new();
        tokio::spawn(run(
            Arc::clone(&self.id),
            vendor.clone(),
            Arc::clone(&self.headamp),
            self.tx.clone(),
            self.config.meters,
            cancel.clone(),
        ));
        self.running = Some(Running { _device: device, vendor, cancel });
        Ok(())
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        if let Some(running) = self.running.take() {
            running.cancel.cancel();
        }
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        self.vendor()?;
        Ok(DeviceInfo {
            vendor: "Yamaha (emulated)".to_string(),
            model: format!("{} as {}", self.config.model.name, self.identity().name),
            address: IpAddr::V4(self.config.interface),
        })
    }

    /// Changes the gain the emulated Rio holds and tells the network, as a
    /// Rio reports a change made at its own end. Rounded to a whole dB and
    /// held to -6..+66, the Rio's step and range.
    async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
        self.check_channel(channel)?;
        let vendor = self.vendor()?.clone();
        let announcement = self.headamp.lock().expect("head-amp state").set_gain_db(channel, gain_db);
        if let Some(announcement) = announcement {
            send_blocks(&vendor, std::slice::from_ref(&announcement)).await;
        }
        Ok(())
    }

    async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
        self.check_channel(channel)?;
        let vendor = self.vendor()?.clone();
        let announcement = self.headamp.lock().expect("head-amp state").set_phantom(channel, on);
        if let Some(announcement) = announcement {
            send_blocks(&vendor, std::slice::from_ref(&announcement)).await;
        }
        Ok(())
    }

    /// What the emulated Rio holds: the last value a console wrote or the
    /// bridge set. Before either, the bottom of the range and +48 V off.
    async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
        self.check_channel(channel)?;
        let device = self.headamp.lock().expect("head-amp state");
        Ok(PreampState {
            gain_db: device.gain_db(channel).ok_or(AdapterError::UnsupportedChannel(channel))?,
            phantom: device.phantom(channel).ok_or(AdapterError::UnsupportedChannel(channel))?,
            pad: None,
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::super::codec::{
        gain_broadcast, phantom_broadcast, wrap_conmon, MbcBlock, FLAG_FROM_DEVICE, FLAG_UNSOLICITED, KIND_READ,
        KIND_WRITE, MAC_BROADCAST, OPCODE_HEADAMP, SUBOP_GAIN,
    };
    use super::super::device::{OPCODE_POLL, SUBOP_POLL};
    use super::super::MESSAGE_CLASS_CONSOLE;
    use super::*;
    use std::net::SocketAddrV4;
    use tokio::net::UdpSocket;

    const CONSOLE_MAC: [u8; 6] = [0x00, 0xa0, 0xde, 0xe0, 0xce, 0xf6];
    const CONSOLE_EUI64: [u8; 8] = [0x00, 0x1d, 0xc1, 0x17, 0xea, 0x2c, 0x00, 0x00];
    const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 232);

    fn config(vendor_port: u16) -> EmulatedRioConfig {
        EmulatedRioConfig {
            advertise: false,
            ports: Ports { arc: 0, cmc: 0, control: 0, vendor: vendor_port },
            ..EmulatedRioConfig::new(Ipv4Addr::LOCALHOST, 5)
        }
    }

    #[test]
    fn the_default_name_is_built_the_way_yamaha_builds_it() {
        let mut config = config(0);
        config.mac = Some([0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04]);
        config.unit_id = 4;
        assert_eq!(config.identity("rio").name, "Y004-Yamaha-Rio3224-D2-25df04");
        config.unit_id = 0x1f;
        assert_eq!(config.identity("rio").name, "Y01F-Yamaha-Rio3224-D2-25df04");

        // With no MAC given, one is derived: locally administered, and the
        // same for the same device on the same interface.
        config.mac = None;
        let a = config.identity("rio");
        assert_eq!(a.mac[0], 0x02);
        assert_eq!(a, config.identity("rio"));
        assert_ne!(a.mac, config.identity("other").mac);
        assert!(a.name.len() <= 31, "a Dante name");
    }

    #[tokio::test]
    async fn it_refuses_channels_it_does_not_have_and_calls_before_it_runs() {
        let mut adapter = EmulatedRioAdapter::new("rio", config(0));
        assert!(matches!(adapter.set_gain(0, 1.0).await, Err(AdapterError::UnsupportedChannel(0))));
        assert!(matches!(adapter.set_gain(33, 1.0).await, Err(AdapterError::UnsupportedChannel(33))));
        assert!(matches!(adapter.set_gain(1, 1.0).await, Err(AdapterError::Connection(_))));
        assert!(adapter.identify().await.is_err());
        // Its state can be read before it runs: it is the adapter's own.
        let state = adapter.get_state(1).await.unwrap();
        assert_eq!((state.gain_db, state.phantom, state.pad), (-6.0, false, None));
    }

    /// A console on the loopback vendor group, for the tests below.
    struct Console {
        socket: UdpSocket,
        group: SocketAddrV4,
        sequence: u16,
    }

    impl Console {
        fn join(port: u16) -> Option<Self> {
            use socket2::{Domain, Protocol, Socket, Type};
            let build = || -> std::io::Result<UdpSocket> {
                let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
                socket.set_reuse_address(true)?;
                #[cfg(unix)]
                socket.set_reuse_port(true)?;
                socket.set_nonblocking(true)?;
                socket.bind(&std::net::SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
                socket.join_multicast_v4(&GROUP, &Ipv4Addr::LOCALHOST)?;
                socket.set_multicast_if_v4(&Ipv4Addr::LOCALHOST)?;
                socket.set_multicast_loop_v4(true)?;
                UdpSocket::from_std(socket.into())
            };
            Some(Self { socket: build().ok()?, group: SocketAddrV4::new(GROUP, port), sequence: 0x0100 })
        }

        async fn send(&mut self, block: &MbcBlock) {
            self.sequence += 1;
            let packet = wrap_conmon(&block.encode(), self.sequence, CONSOLE_EUI64, MESSAGE_CLASS_CONSOLE);
            self.socket.send_to(&packet, self.group).await.unwrap();
        }

        /// The next block from a device that `want` accepts.
        async fn expect(&self, what: &str, want: impl Fn(&MbcBlock) -> bool) -> MbcBlock {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut buf = [0u8; 2048];
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (len, _) = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf))
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                    .unwrap();
                for block in blocks_in_conmon_payload(&buf[..len]) {
                    if block.flags & (FLAG_FROM_DEVICE | FLAG_UNSOLICITED) != 0 && want(&block) {
                        return block;
                    }
                }
            }
        }
    }

    /// Starts an emulated Rio on loopback with a console beside it. `None`
    /// where loopback carries no multicast (a stock Linux `lo`): the test is
    /// skipped there, not failed.
    async fn bench(port: u16) -> Option<(EmulatedRioAdapter, Console)> {
        // Joining the group can succeed on a machine that then delivers
        // nothing, so ask first: one socket sends, another must hear it.
        let delivers = match (Console::join(port), Console::join(port)) {
            (Some(a), Some(b)) => {
                let mut buf = [0u8; 16];
                a.socket.send_to(b"probe", a.group).await.is_ok()
                    && tokio::time::timeout(Duration::from_millis(500), b.socket.recv_from(&mut buf)).await.is_ok()
            }
            _ => false,
        };
        if !delivers {
            eprintln!("skipping: loopback does not carry multicast here");
            return None;
        }
        let console = Console::join(port)?;
        let mut adapter = EmulatedRioAdapter::new("virtual-rio", config(port));
        if let Err(e) = adapter.connect().await {
            eprintln!("skipping: {e}");
            return None;
        }
        Some((adapter, console))
    }

    fn read(dst_mac: [u8; 6], opcode: u16, subop: u8, count: u16) -> MbcBlock {
        MbcBlock {
            version: 1,
            src_mac: CONSOLE_MAC,
            dst_mac,
            flags: KIND_READ,
            opcode,
            subop,
            count,
            start_index: 0,
            data: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_console_finds_it_writes_gain_and_the_bridge_hears_about_it() {
        let Some((mut adapter, mut console)) = bench(48721).await else { return };
        let mut events = adapter.subscribe();
        let device_mac = adapter.identity().mac;

        // The power-up announcement names the device.
        let announced = console.expect("the announcement", |b| (b.opcode, b.subop) == (0x0711, 0x05)).await;
        assert_eq!(announced.src_mac, device_mac);
        assert!(announced.data.starts_with(b"Y005-Yamaha-Rio3224-D2-"));

        // The console polls, to everyone; the device answers to the console.
        console.send(&read(MAC_BROADCAST, OPCODE_POLL, SUBOP_POLL, 1)).await;
        let found = console.expect("the poll answer", |b| (b.opcode, b.subop) == (OPCODE_POLL, SUBOP_POLL)).await;
        assert_eq!(found.dst_mac, CONSOLE_MAC);
        assert_eq!(&found.data[..2], &[0x00, 0x05], "unit id 5");
        assert_eq!(&found.data[2..12], b"Rio3224-D2");

        // The console writes its scene: input 3 at +24 dB, the rest at -6.
        let mut gains = [-6.0f32; HEADAMP_CHANNELS];
        gains[2] = 24.0;
        console.send(&gain_broadcast(CONSOLE_MAC, device_mac, &gains)).await;
        let echo = console
            .expect("the gain echo", |b| b.flags == (FLAG_FROM_DEVICE | KIND_WRITE) && b.subop == SUBOP_GAIN)
            .await;
        assert_eq!(echo.gain_db().unwrap()[2], 24.0);

        // One channel changed, so the bridge hears of one channel, and of
        // its gain only: +48 V was not touched and must not be relayed.
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv()).await.expect("an event").unwrap();
        assert_eq!(event.address, PreampAddress::new("virtual-rio", 3));
        assert_eq!(event.state.gain_db, 24.0);
        assert_eq!(event.changed, ChangedFields::GAIN);
        assert!(events.try_recv().is_err(), "31 channels did not move");
        assert_eq!(adapter.get_state(3).await.unwrap().gain_db, 24.0);

        // +48 V on input 3.
        let mut phantom = [false; HEADAMP_CHANNELS];
        phantom[2] = true;
        console.send(&phantom_broadcast(CONSOLE_MAC, device_mac, &phantom)).await;
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv()).await.expect("an event").unwrap();
        assert_eq!(event.changed, ChangedFields::PHANTOM);
        assert!(event.state.phantom);
        assert_eq!(event.state.gain_db, 24.0, "carried, not reported");

        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn a_change_from_the_bridge_is_announced_to_the_console() {
        let Some((mut adapter, console)) = bench(48722).await else { return };
        let mut events = adapter.subscribe();

        adapter.set_gain(7, 30.0).await.unwrap();
        let announced = console
            .expect("the gain announcement", |b| {
                b.flags == FLAG_UNSOLICITED && (b.opcode, b.subop) == (OPCODE_HEADAMP, SUBOP_GAIN)
            })
            .await;
        assert_eq!(announced.dst_mac, MAC_BROADCAST);
        let gains = announced.gain_db().unwrap();
        assert_eq!(gains[6], 30.0);
        assert_eq!(gains[0], -6.0);

        // The bridge's own write is not reported back to the bridge: only a
        // console's moves are events. Otherwise a two-way mapping would
        // chase its own tail.
        assert!(tokio::time::timeout(Duration::from_millis(200), events.recv()).await.is_err());
        adapter.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn metering_starts_when_asked_for() {
        let Some((mut adapter, mut console)) = bench(48723).await else { return };
        let device_mac = adapter.identity().mac;
        let mut request = read(device_mac, 0x0742, 0x00, 32);
        request.flags = 0x03;
        console.send(&request).await;
        console.expect("the metering acknowledgement", |b| b.flags == 0x23).await;
        let frame = console.expect("a metering frame", |b| b.flags == 0x13).await;
        assert_eq!(frame.data, vec![0x1f; 32], "silence, at the Rio's floor");
        adapter.disconnect().await.unwrap();
    }
}

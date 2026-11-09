//! A running virtual device: the sockets, and the tasks that answer on them.
//!
//! All the protocol knowledge is in [`crate::arc`], [`crate::cmc`] and
//! [`crate::status`]. This module only moves datagrams between sockets and
//! those functions.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::arc::{self, ArcState};
use crate::cmc;
use crate::conmon;
use crate::identity::{DeviceIdentity, IdentityError, MacDisplay};
use crate::mdns::{AdvertiseError, Advertisement};
use crate::persona::Persona;
use crate::status;

/// A controller renews its ConMon registration about once a second. One
/// that has said nothing for this long has gone.
const SUBSCRIBER_TIMEOUT: Duration = Duration::from_secs(10);
/// Gap between the status messages published at start-up. The Rio spaced
/// its own by tens of milliseconds.
const ANNOUNCE_GAP: Duration = Duration::from_millis(40);
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(1);
/// Every this many heartbeats, the standing status messages go out again.
const REPUBLISH_EVERY: u64 = 4;

#[derive(Debug, thiserror::Error)]
pub enum EmulationError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("{what} on {address}: {source}")]
    Socket {
        what: &'static str,
        address: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Advertise(#[from] AdvertiseError),
}

/// UDP ports for the device's own endpoints. `0` lets the system choose.
///
/// The unicast ports are not fixed by Dante: ARC and ConMon registration
/// ports are published over mDNS, and the control port is given in the
/// registration reply. Only the vendor channel's port is a constant of the
/// protocol, and it is here so tests can move it out of the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub arc: u16,
    pub cmc: u16,
    pub control: u16,
    pub vendor: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            arc: arc::DEFAULT_PORT,
            cmc: conmon::CMC_PORT,
            control: conmon::CONTROL_PORT,
            vendor: conmon::VENDOR_PORT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VirtualDeviceConfig {
    pub identity: DeviceIdentity,
    pub persona: Persona,
    /// Answer as a Dante device: ARC, ConMon registration and status.
    /// With this off only the vendor channel runs.
    pub presence: bool,
    /// Advertise over mDNS. Needs `presence`.
    pub advertise: bool,
    pub ports: Ports,
}

impl VirtualDeviceConfig {
    pub fn new(identity: DeviceIdentity, persona: Persona) -> Self {
        Self { identity, persona, presence: true, advertise: true, ports: Ports::default() }
    }
}

/// A datagram heard on the vendor channel.
#[derive(Debug, Clone)]
pub struct VendorDatagram {
    pub from: SocketAddr,
    pub payload: Vec<u8>,
}

/// The device's seat on the ConMon vendor channel (224.0.0.232:8705), where
/// a console's own control protocol travels.
///
/// The channel carries whole datagrams and does not look inside them. The
/// vendor's adapter builds each packet, taking the device id and the next
/// sequence number from here so that every packet the device sends counts
/// on one sequence, as a real device's do.
#[derive(Clone)]
pub struct VendorChannel {
    socket: Arc<UdpSocket>,
    destination: SocketAddrV4,
    device_id: [u8; 8],
    sequence: Arc<AtomicU16>,
    incoming: broadcast::Sender<VendorDatagram>,
}

impl VendorChannel {
    /// The ConMon device id to put in the envelope.
    pub fn device_id(&self) -> [u8; 8] {
        self.device_id
    }

    /// The sequence number for the next packet. The first is 0, which is
    /// what the Rio's first vendor packet carried.
    pub fn next_sequence(&self) -> u16 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends one packet to the vendor group.
    pub async fn send(&self, packet: &[u8]) -> io::Result<()> {
        self.socket.send_to(packet, SocketAddr::V4(self.destination)).await.map(|_| ())
    }

    /// Everything other devices send on the vendor channel. The device's
    /// own packets, which multicast loopback hands back, are left out.
    pub fn subscribe(&self) -> broadcast::Receiver<VendorDatagram> {
        self.incoming.subscribe()
    }
}

struct Subscriber {
    channel: u16,
    address: SocketAddrV4,
    seen: Instant,
}

struct Shared {
    identity: DeviceIdentity,
    persona: Persona,
    /// What controllers have set on the device. Never held across an await.
    state: Mutex<ArcState>,
    status_socket: UdpSocket,
    status_sequence: AtomicU16,
    monitor_sequence: AtomicU16,
    subscribers: Mutex<Vec<Subscriber>>,
}

impl Shared {
    /// Publishes one status message to the status group, and to every
    /// controller registered for the status channel.
    async fn publish(&self, message_type: u16) {
        let sequence = self.status_sequence.fetch_add(1, Ordering::Relaxed);
        let encoding_bits = self.state.lock().expect("device state").encoding_bits();
        let Some(packet) =
            status::message(message_type, sequence, &self.identity, &self.persona, encoding_bits)
        else {
            return;
        };
        let group = SocketAddrV4::new(conmon::STATUS_GROUP, conmon::STATUS_PORT);
        if let Err(e) = self.status_socket.send_to(&packet, group).await {
            debug!(error = %e, "status multicast failed");
        }
        for address in self.live_subscribers(cmc::CHANNEL_STATUS) {
            if let Err(e) = self.status_socket.send_to(&packet, address).await {
                debug!(%address, error = %e, "status unicast failed");
            }
        }
    }

    fn live_subscribers(&self, channel: u16) -> Vec<SocketAddrV4> {
        let mut subscribers = self.subscribers.lock().expect("subscriber list");
        subscribers.retain(|s| s.seen.elapsed() < SUBSCRIBER_TIMEOUT);
        subscribers.iter().filter(|s| s.channel == channel).map(|s| s.address).collect()
    }

    fn note_subscription(&self, subscription: cmc::Subscription, from: SocketAddr) {
        let mut subscribers = self.subscribers.lock().expect("subscriber list");
        if subscription.is_withdrawal() {
            // A withdrawal carries no address, so it is matched by where it
            // came from.
            subscribers.retain(|s| {
                !(s.channel == subscription.channel && SocketAddr::V4(s.address).ip() == from.ip())
            });
            return;
        }
        match subscribers
            .iter_mut()
            .find(|s| s.channel == subscription.channel && s.address == subscription.address)
        {
            Some(existing) => existing.seen = Instant::now(),
            None => {
                info!(channel = subscription.channel, address = %subscription.address, "controller registered for a ConMon channel");
                subscribers.push(Subscriber {
                    channel: subscription.channel,
                    address: subscription.address,
                    seen: Instant::now(),
                });
            }
        }
    }
}

/// A virtual Dante device, running until [`shutdown`](Self::shutdown) or
/// drop.
pub struct VirtualDevice {
    identity: DeviceIdentity,
    ports: Ports,
    vendor: VendorChannel,
    cancel: CancellationToken,
    _advertisement: Option<Advertisement>,
}

impl VirtualDevice {
    /// Binds the device's sockets and starts answering.
    pub async fn start(config: VirtualDeviceConfig) -> Result<Self, EmulationError> {
        crate::identity::validate_name(&config.identity.name)?;
        let VirtualDeviceConfig { identity, persona, presence, advertise, ports } = config;
        let cancel = CancellationToken::new();

        let vendor_socket = bind_group(conmon::VENDOR_GROUP, ports.vendor, identity.ip)?;
        let (incoming, _) = broadcast::channel(256);
        let mut actual = Ports { vendor: local_port(&vendor_socket), ..ports };
        let vendor = VendorChannel {
            socket: Arc::new(vendor_socket),
            destination: SocketAddrV4::new(conmon::VENDOR_GROUP, actual.vendor),
            device_id: identity.eui64(),
            sequence: Arc::new(AtomicU16::new(0)),
            incoming,
        };
        tokio::spawn(vendor_loop(vendor.clone(), cancel.clone()));

        let mut advertisement = None;
        if presence {
            let arc_socket = bind_unicast("binding the ARC port", identity.ip, ports.arc)?;
            let cmc_socket = bind_unicast("binding the ConMon registration port", identity.ip, ports.cmc)?;
            let control_socket = bind_unicast("binding the ConMon control port", identity.ip, ports.control)?;
            let status_socket = bind_sender(identity.ip)?;
            actual.arc = local_port(&arc_socket);
            actual.cmc = local_port(&cmc_socket);
            actual.control = local_port(&control_socket);

            let shared = Arc::new(Shared {
                identity: identity.clone(),
                persona: persona.clone(),
                state: Mutex::new(ArcState::new(&persona)),
                status_socket,
                status_sequence: AtomicU16::new(0),
                monitor_sequence: AtomicU16::new(0),
                subscribers: Mutex::new(Vec::new()),
            });

            tokio::spawn(arc_loop(arc_socket, Arc::clone(&shared), cancel.clone()));
            tokio::spawn(cmc_loop(cmc_socket, Arc::clone(&shared), actual.control, cancel.clone()));
            tokio::spawn(control_loop(control_socket, Arc::clone(&shared), cancel.clone()));
            tokio::spawn(announce_loop(Arc::clone(&shared), cancel.clone()));

            if advertise {
                advertisement = Some(Advertisement::start(&identity, &persona, actual.arc, actual.cmc)?);
            }
        }

        info!(
            name = %identity.name, mac = %MacDisplay(identity.mac), address = %identity.ip,
            arc = actual.arc, cmc = actual.cmc, control = actual.control, presence, advertise,
            "virtual Dante device running"
        );
        Ok(Self { identity, ports: actual, vendor, cancel, _advertisement: advertisement })
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// The ports actually bound, which differ from the ones asked for when a
    /// standard port was taken or `0` was given.
    pub fn ports(&self) -> Ports {
        self.ports
    }

    pub fn vendor(&self) -> VendorChannel {
        self.vendor.clone()
    }

    /// Stops answering and withdraws the mDNS records.
    pub fn shutdown(self) {
        // Drop does the work; this exists so call sites read as intended.
    }
}

impl Drop for VirtualDevice {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn local_port(socket: &UdpSocket) -> u16 {
    socket.local_addr().map(|a| a.port()).unwrap_or(0)
}

fn socket_error(what: &'static str, address: SocketAddr) -> impl FnOnce(io::Error) -> EmulationError {
    move |source| EmulationError::Socket { what, address, source }
}

/// A unicast port on the device's own address. If the standard port is
/// taken (Dante Virtual Soundcard holds 4440 and 8800 on a machine that runs
/// it), the system picks another: these ports are published, not assumed,
/// so a controller finds the device either way.
fn bind_unicast(what: &'static str, ip: Ipv4Addr, port: u16) -> Result<UdpSocket, EmulationError> {
    let address = SocketAddr::from((ip, port));
    let std_socket = match std::net::UdpSocket::bind(address) {
        Ok(socket) => socket,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse && port != 0 => {
            warn!(%address, "port in use, letting the system choose another");
            std::net::UdpSocket::bind(SocketAddr::from((ip, 0))).map_err(socket_error(what, address))?
        }
        Err(e) => return Err(socket_error(what, address)(e)),
    };
    std_socket.set_nonblocking(true).map_err(socket_error(what, address))?;
    UdpSocket::from_std(std_socket).map_err(socket_error(what, address))
}

fn multicast_options(socket: &Socket, interface: Ipv4Addr) -> io::Result<()> {
    // TTL 1: ConMon is link-local and must not be routed off the segment.
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_if_v4(&interface)?;
    // On, so a controller on this same machine hears the device.
    socket.set_multicast_loop_v4(true)?;
    Ok(())
}

/// The socket status and heartbeat messages are sent from.
fn bind_sender(interface: Ipv4Addr) -> Result<UdpSocket, EmulationError> {
    let address = SocketAddr::from((interface, 0));
    let build = || -> io::Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_nonblocking(true)?;
        socket.bind(&address.into())?;
        multicast_options(&socket, interface)?;
        UdpSocket::from_std(socket.into())
    };
    build().map_err(socket_error("opening the status socket", address))
}

/// A socket on a ConMon multicast group, shared with whatever else on this
/// machine listens to the same group (Dante Controller does).
fn bind_group(group: Ipv4Addr, port: u16, interface: Ipv4Addr) -> Result<UdpSocket, EmulationError> {
    let address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    let build = || -> io::Result<UdpSocket> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&address.into())?;
        socket.join_multicast_v4(&group, &interface)?;
        multicast_options(&socket, interface)?;
        UdpSocket::from_std(socket.into())
    };
    build().map_err(socket_error("joining the ConMon vendor group", address))
}

async fn vendor_loop(channel: VendorChannel, cancel: CancellationToken) {
    let mut buf = [0u8; 2048];
    loop {
        let (len, from) = tokio::select! {
            _ = cancel.cancelled() => return,
            result = channel.socket.recv_from(&mut buf) => match result {
                Ok(received) => received,
                Err(e) => {
                    // A UDP socket can report a stray ICMP error here; the
                    // socket itself is still good.
                    debug!(error = %e, "vendor channel read failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let payload = &buf[..len];
        if let Some((header, _)) = conmon::parse(payload) {
            if header.device_id == channel.device_id {
                continue; // our own packet, looped back
            }
        }
        let _ = channel.incoming.send(VendorDatagram { from, payload: payload.to_vec() });
    }
}

async fn arc_loop(socket: UdpSocket, shared: Arc<Shared>, cancel: CancellationToken) {
    let mut buf = [0u8; 2048];
    loop {
        let (len, from) = tokio::select! {
            _ = cancel.cancelled() => return,
            result = socket.recv_from(&mut buf) => match result {
                Ok(received) => received,
                Err(e) => {
                    debug!(error = %e, "ARC read failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let request = &buf[..len];
        let reply = {
            let mut state = shared.state.lock().expect("device state");
            arc::respond(request, &shared.identity, &shared.persona, &mut state)
        };
        match reply {
            Some(reply) => {
                if let Err(e) = socket.send_to(&reply, from).await {
                    debug!(%from, error = %e, "ARC reply failed");
                }
            }
            None => {
                // Worth seeing: an opcode nobody has captured a reply for is
                // the first thing to look at when a controller will not
                // accept the device.
                let head: String = request.iter().take(16).map(|b| format!("{b:02x}")).collect();
                info!(%from, len, %head, "ARC request with no known reply");
            }
        }
    }
}

async fn cmc_loop(socket: UdpSocket, shared: Arc<Shared>, control_port: u16, cancel: CancellationToken) {
    let mut buf = [0u8; 2048];
    loop {
        let (len, from) = tokio::select! {
            _ = cancel.cancelled() => return,
            result = socket.recv_from(&mut buf) => match result {
                Ok(received) => received,
                Err(e) => {
                    debug!(error = %e, "ConMon registration read failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let request = &buf[..len];
        match cmc::respond(request, &shared.identity, control_port) {
            Some(reply) => {
                if let Some(subscription) = reply.subscription {
                    shared.note_subscription(subscription, from);
                }
                if let Err(e) = socket.send_to(&reply.packet, from).await {
                    debug!(%from, error = %e, "ConMon registration reply failed");
                }
            }
            None => {
                let head: String = request.iter().take(16).map(|b| format!("{b:02x}")).collect();
                info!(%from, len, %head, "ConMon registration request with no known reply");
            }
        }
    }
}

async fn control_loop(socket: UdpSocket, shared: Arc<Shared>, cancel: CancellationToken) {
    let mut buf = [0u8; 2048];
    loop {
        let (len, from) = tokio::select! {
            _ = cancel.cancelled() => return,
            result = socket.recv_from(&mut buf) => match result {
                Ok(received) => received,
                Err(e) => {
                    debug!(error = %e, "ConMon control read failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let Some((header, body)) = conmon::parse(&buf[..len]) else {
            continue;
        };
        if let Some(bits) = status::encoding_write(header.message_type(), body) {
            info!(%from, bits, "controller set the sample encoding");
            shared.state.lock().expect("device state").set_encoding_bits(bits);
        }
        match status::status_for_query(header.message_type()) {
            Some(message_type) => shared.publish(message_type).await,
            None => {
                info!(%from, query = format!("{:#06x}", header.message_type()), "ConMon query with no known reply");
            }
        }
    }
}

async fn announce_loop(shared: Arc<Shared>, cancel: CancellationToken) {
    for message_type in status::BOOT_ANNOUNCE {
        shared.publish(message_type).await;
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(ANNOUNCE_GAP) => {}
        }
    }

    let monitor = SocketAddrV4::new(conmon::MONITOR_GROUP, conmon::MONITOR_PORT);
    let mut ticker = tokio::time::interval(HEARTBEAT_PERIOD);
    let mut beats: u64 = 0;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let sequence = shared.monitor_sequence.fetch_add(1, Ordering::Relaxed);
        let packet = status::heartbeat(sequence, beats as u16, &shared.identity);
        if let Err(e) = shared.status_socket.send_to(&packet, monitor).await {
            debug!(error = %e, "heartbeat failed");
        }
        beats += 1;
        if beats.is_multiple_of(REPUBLISH_EVERY) {
            for message_type in status::PERIODIC {
                shared.publish(message_type).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hexutil::hex;

    fn config(vendor_port: u16) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            identity: DeviceIdentity {
                name: "Y004-Yamaha-Rio3224-D2-25df04".into(),
                mac: [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04],
                ip: Ipv4Addr::LOCALHOST,
            },
            persona: Persona::yamaha_rio3224_d2(),
            presence: true,
            advertise: false,
            ports: Ports { arc: 0, cmc: 0, control: 0, vendor: vendor_port },
        }
    }

    /// Starts a device on loopback, or says why it could not. Loopback does
    /// not carry multicast everywhere (a stock Linux `lo` has no MULTICAST
    /// flag), and a test that needs it is skipped there, not failed.
    async fn start(vendor_port: u16) -> Option<VirtualDevice> {
        match VirtualDevice::start(config(vendor_port)).await {
            Ok(device) => Some(device),
            Err(e) => {
                eprintln!("skipping: no multicast on loopback here ({e})");
                None
            }
        }
    }

    async fn exchange(socket: &UdpSocket, to: SocketAddr, request: &[u8]) -> Vec<u8> {
        socket.send_to(request, to).await.unwrap();
        let mut buf = [0u8; 2048];
        let (len, _) = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf))
            .await
            .expect("a reply within two seconds")
            .unwrap();
        buf[..len].to_vec()
    }

    #[tokio::test]
    async fn arc_and_registration_answer_over_real_sockets() {
        let Some(device) = start(48705).await else { return };
        let ports = device.ports();
        assert_ne!(ports.arc, 0);
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let count = exchange(&client, (Ipv4Addr::LOCALHOST, ports.arc).into(), &hex("2801000a001710000000")).await;
        assert_eq!(&count[10..16], &hex("0ff900200018")[..], "32 transmit and 24 receive channels");

        // "Where is your control port?" must give the port actually bound.
        let locate =
            exchange(&client, (Ipv4Addr::LOCALHOST, ports.cmc).into(), &hex("120000140002100100000000001dc117ea2c0000")).await;
        assert_eq!(&locate[24..28], &[127, 0, 0, 1]);
        assert_eq!(u16::from_be_bytes([locate[28], locate[29]]), ports.control);
    }

    #[tokio::test]
    async fn a_registered_controller_is_sent_the_status_it_asks_for() {
        let Some(device) = start(48706).await else { return };
        let ports = device.ports();
        let controller = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let status_port = controller.local_addr().unwrap().port();

        // Register for the status channel at our own port, as the console
        // registered at its port 8702.
        let mut subscribe = hex(
            "120000680000301000000000001dc117ea2c00000004002800010040000a\
             593030342d59616d6168612d52696f333232342d44322d32356466303400\
             593030312d59616d6168612d514c312d3137656132630000000100440002000000010000a9feb10d21fe0000",
        );
        let n = subscribe.len();
        subscribe[n - 8..n - 4].copy_from_slice(&[127, 0, 0, 1]);
        subscribe[n - 4..n - 2].copy_from_slice(&status_port.to_be_bytes());
        let ack = exchange(&controller, (Ipv4Addr::LOCALHOST, ports.cmc).into(), &subscribe).await;
        assert_eq!(&ack[8..10], &[0, 1]);

        // Ask for make and model, as the console did with query 0x00c1.
        let query = hex("ffff002000440000001dc117ea2c0000417564696e617465072e00c1000f4240");
        controller.send_to(&query, (Ipv4Addr::LOCALHOST, ports.control)).await.unwrap();

        // Start-up announcements may arrive first; wait for the one asked for.
        let mut buf = [0u8; 2048];
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (len, _) = tokio::time::timeout(remaining, controller.recv_from(&mut buf))
                .await
                .expect("the make-and-model status")
                .unwrap();
            let (header, body) = conmon::parse(&buf[..len]).expect("a ConMon message");
            if header.message_type() == status::TYPE_MAKE_MODEL {
                assert_eq!(header.device_id, device.identity().eui64());
                assert_eq!(&body[4..10], b"Yamaha");
                assert_eq!(&body[176..186], b"Rio3224-D2");
                break;
            }
        }
    }

    /// Whether a datagram sent to the vendor group on loopback comes back.
    /// Joining the group can succeed on a machine that then delivers
    /// nothing, so the tests that need delivery ask first.
    async fn loopback_carries_multicast(port: u16) -> bool {
        let (Ok(a), Ok(b)) = (
            bind_group(conmon::VENDOR_GROUP, port, Ipv4Addr::LOCALHOST),
            bind_group(conmon::VENDOR_GROUP, port, Ipv4Addr::LOCALHOST),
        ) else {
            return false;
        };
        if a.send_to(b"probe", SocketAddrV4::new(conmon::VENDOR_GROUP, port)).await.is_err() {
            return false;
        }
        let mut buf = [0u8; 16];
        tokio::time::timeout(Duration::from_millis(500), b.recv_from(&mut buf)).await.is_ok()
    }

    #[tokio::test]
    async fn the_vendor_channel_carries_packets_both_ways_and_hides_its_own() {
        if !loopback_carries_multicast(48709).await {
            eprintln!("skipping: loopback does not carry multicast here");
            return;
        }
        let Some(device) = start(48707).await else { return };
        let vendor = device.vendor();
        let mut heard = vendor.subscribe();
        let group = SocketAddrV4::new(conmon::VENDOR_GROUP, device.ports().vendor);

        // Another device on the group: a console, say.
        let console = match bind_group(conmon::VENDOR_GROUP, device.ports().vendor, Ipv4Addr::LOCALHOST) {
            Ok(socket) => socket,
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };
        let from_console = conmon::encode(
            &conmon::Header {
                magic: conmon::MAGIC,
                sequence: 7,
                process: 0,
                device_id: [0x00, 0x1d, 0xc1, 0x17, 0xea, 0x2c, 0x00, 0x00],
                vendor: conmon::VENDOR_AUDINATE,
                class: 0x072e_1002,
            },
            b"hello",
        );
        console.send_to(&from_console, group).await.unwrap();
        let datagram = tokio::time::timeout(Duration::from_secs(2), heard.recv()).await.expect("the console's packet").unwrap();
        assert_eq!(datagram.payload, from_console);

        // The device's own packet reaches the console and is not handed
        // back to the device.
        assert_eq!(vendor.next_sequence(), 0);
        let from_device = conmon::encode(
            &conmon::Header {
                magic: conmon::MAGIC,
                sequence: 0,
                process: 0,
                device_id: vendor.device_id(),
                vendor: conmon::VENDOR_AUDINATE,
                class: 0x0731_1002,
            },
            b"reply",
        );
        vendor.send(&from_device).await.unwrap();
        let mut buf = [0u8; 256];
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (len, _) = tokio::time::timeout(remaining, console.recv_from(&mut buf)).await.expect("the device's packet").unwrap();
            if buf[..len] == from_device[..] {
                break;
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), heard.recv()).await.is_err(),
            "the device must not hear itself"
        );
    }

    #[tokio::test]
    async fn a_bad_name_is_refused_before_any_socket_is_opened() {
        let mut config = config(48708);
        config.identity.name = "not a dante name".into();
        assert!(matches!(VirtualDevice::start(config).await, Err(EmulationError::Identity(_))));
    }
}

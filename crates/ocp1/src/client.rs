//! An OCP.1 controller: TCP connection, request/response correlation,
//! subscriptions and the keepalive heartbeat.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::classes::{root, single_value, subscription, ClassIdentification};
use crate::ono::{reserved, Ono};
use crate::pdu::{self, Command, MemberId, Message, Notification, Response};
use crate::value::{Reader, Writer};
use crate::Error;

/// AES70-3 does not assign OCP.1 a port — the standard expects discovery via
/// mDNS (`_oca._tcp`), and that is what a caller should prefer. 65000 is the
/// widely-used convention and is offered only as a fallback for a device whose
/// port is already known.
pub const DEFAULT_OCP1_PORT: u16 = 65000;

const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(5);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const NOTIFICATION_BUFFER: usize = 256;

type Pending = Arc<Mutex<HashMap<u32, oneshot::Sender<Response>>>>;

/// Aborts the connection's background tasks when the last [`Client`] handle
/// goes away.
///
/// Without this, dropping a `Client` closes only the write half — the read task
/// owns the read half and stays parked in `read()` until the *device* hangs up,
/// holding the socket and its subscriptions open. A host that removes a device
/// live would leak one of each every time.
struct Tasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

pub struct Client {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    notifications: broadcast::Sender<Notification>,
    next_handle: AtomicU32,
    timeout: Duration,
    /// Shared, so a handle from [`Client::set_timeout`] keeps the connection
    /// alive rather than tearing it down when it drops.
    tasks: Arc<Tasks>,
}

impl Client {
    /// Connect and start the reader and heartbeat tasks.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Arc<Self>, Error> {
        let stream = TcpStream::connect(addr).await?;
        // Control traffic is small and latency-sensitive; batching it helps nobody.
        stream.set_nodelay(true)?;
        Ok(Self::from_stream(stream, DEFAULT_HEARTBEAT))
    }

    pub fn from_stream(stream: TcpStream, heartbeat: Duration) -> Arc<Self> {
        let (read_half, mut write_half) = stream.into_split();
        let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (notifications, _) = broadcast::channel(NOTIFICATION_BUFFER);
        let pending: Pending = Arc::default();

        let writer = tokio::spawn(async move {
            while let Some(bytes) = outgoing_rx.recv().await {
                if write_half.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });

        let reader = tokio::spawn(read_loop(read_half, pending.clone(), notifications.clone()));

        let outgoing_for_heartbeat = outgoing.clone();
        let heartbeats = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(heartbeat);
            loop {
                ticker.tick().await;
                let secs = heartbeat.as_secs().clamp(1, u16::MAX as u64) as u16;
                if outgoing_for_heartbeat.send(pdu::encode_keepalive(secs)).is_err() {
                    break;
                }
            }
        });

        Arc::new(Self {
            outgoing,
            pending,
            notifications,
            next_handle: AtomicU32::new(1),
            timeout: DEFAULT_TIMEOUT,
            tasks: Arc::new(Tasks(vec![writer, reader, heartbeats])),
        })
    }

    pub fn set_timeout(self: &Arc<Self>, timeout: Duration) -> Arc<Self> {
        // Timeout is per-client and set at construction time in practice; this
        // rebuilds the handle rather than making the field interior-mutable.
        Arc::new(Self {
            outgoing: self.outgoing.clone(),
            pending: self.pending.clone(),
            notifications: self.notifications.clone(),
            next_handle: AtomicU32::new(self.next_handle.load(Ordering::Relaxed)),
            timeout,
            tasks: Arc::clone(&self.tasks),
        })
    }

    /// Subscribe to every property-changed notification the device sends us.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Send a command and wait for the matching response.
    pub async fn request(
        &self,
        target: Ono,
        method: MemberId,
        param_count: u8,
        params: Vec<u8>,
    ) -> Result<Response, Error> {
        // Handle 0 is reserved as "no handle", so skip it on wraparound.
        let mut handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        if handle == 0 {
            handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        }

        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending mutex poisoned").insert(handle, tx);

        let cmd = Command { handle, target: target.0, method, param_count, params };
        if self.outgoing.send(pdu::encode_command(&cmd, true)).is_err() {
            self.pending.lock().expect("pending mutex poisoned").remove(&handle);
            return Err(Error::Disconnected);
        }

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(response)) => response.into_result(),
            Ok(Err(_)) => Err(Error::Disconnected),
            Err(_) => {
                self.pending.lock().expect("pending mutex poisoned").remove(&handle);
                Err(Error::Timeout)
            }
        }
    }

    /// Read a single-value property (`OcaGain::Gain`, `OcaMute::State`, …).
    pub async fn get(&self, target: Ono, def_level: u16) -> Result<Vec<u8>, Error> {
        let r = self.request(target, single_value::get(def_level), 0, Vec::new()).await?;
        Ok(r.params)
    }

    pub async fn get_f32(&self, target: Ono, def_level: u16) -> Result<f32, Error> {
        Reader::new(&self.get(target, def_level).await?).f32()
    }

    pub async fn get_u8(&self, target: Ono, def_level: u16) -> Result<u8, Error> {
        Reader::new(&self.get(target, def_level).await?).u8()
    }

    pub async fn get_u16(&self, target: Ono, def_level: u16) -> Result<u16, Error> {
        Reader::new(&self.get(target, def_level).await?).u16()
    }

    pub async fn get_i32(&self, target: Ono, def_level: u16) -> Result<i32, Error> {
        Reader::new(&self.get(target, def_level).await?).i32()
    }

    pub async fn get_string(&self, target: Ono, def_level: u16) -> Result<String, Error> {
        Reader::new(&self.get(target, def_level).await?).string()
    }

    async fn set(&self, target: Ono, def_level: u16, params: Vec<u8>) -> Result<(), Error> {
        self.request(target, single_value::set(def_level), 1, params).await?;
        Ok(())
    }

    pub async fn set_f32(&self, target: Ono, def_level: u16, value: f32) -> Result<(), Error> {
        let mut params = Writer::new();
        params.f32(value);
        self.set(target, def_level, params.finish()).await
    }

    pub async fn set_u8(&self, target: Ono, def_level: u16, value: u8) -> Result<(), Error> {
        let mut params = Writer::new();
        params.u8(value);
        self.set(target, def_level, params.finish()).await
    }

    pub async fn set_u16(&self, target: Ono, def_level: u16, value: u16) -> Result<(), Error> {
        let mut params = Writer::new();
        params.u16(value);
        self.set(target, def_level, params.finish()).await
    }

    pub async fn set_i32(&self, target: Ono, def_level: u16, value: i32) -> Result<(), Error> {
        let mut params = Writer::new();
        params.i32(value);
        self.set(target, def_level, params.finish()).await
    }

    pub async fn set_string(&self, target: Ono, def_level: u16, value: &str) -> Result<(), Error> {
        let mut params = Writer::new();
        params.string(value);
        self.set(target, def_level, params.finish()).await
    }

    /// `OcaRoot::GetClassIdentification` — the cheapest probe that tells us
    /// whether an ONo exists and what it is.
    pub async fn class_of(&self, target: Ono) -> Result<ClassIdentification, Error> {
        let r = self.request(target, root::GET_CLASS_IDENTIFICATION, 0, Vec::new()).await?;
        let mut reader = Reader::new(&r.params);
        let fields = reader.list(|r| r.u16())?;
        // Some devices omit the version when it is 1.
        let version = reader.u32().unwrap_or(1);
        Ok(ClassIdentification { fields, version })
    }

    /// `OcaRoot::GetRole` — a device that names its objects descriptively is how
    /// otherwise-undocumented parameters get identified.
    pub async fn role_of(&self, target: Ono) -> Result<String, Error> {
        let r = self.request(target, root::GET_ROLE, 0, Vec::new()).await?;
        Reader::new(&r.params).string()
    }

    /// Ask the device to send us notifications whenever `target`'s property
    /// changes. The device pushes to the same TCP connection.
    pub async fn subscribe(&self, target: Ono) -> Result<(), Error> {
        self.request(
            reserved::SUBSCRIPTION_MANAGER,
            subscription::ADD_SUBSCRIPTION,
            5,
            subscription_params(target, true),
        )
        .await?;
        Ok(())
    }

    pub async fn unsubscribe(&self, target: Ono) -> Result<(), Error> {
        self.request(
            reserved::SUBSCRIPTION_MANAGER,
            subscription::REMOVE_SUBSCRIPTION,
            2,
            subscription_params(target, false),
        )
        .await?;
        Ok(())
    }
}

/// Parameters for `AddSubscription` / `RemoveSubscription`.
///
/// `AddSubscription(event, subscriber, subscriberContext, notificationDeliveryMode,
/// destinationInformation)`; the remove form takes only the event and subscriber.
fn subscription_params(target: Ono, add: bool) -> Vec<u8> {
    let mut w = Writer::new();
    // OcaEvent: emitter ONo + event id (OcaRoot::PropertyChanged is 1.1).
    w.u32(target.0).u16(1).u16(1);
    // Subscriber method: ONo 0 with a method id the device echoes back to us.
    w.u32(0).u16(0).u16(0);
    if add {
        w.bytes(&[]) // subscriber context
            .u8(1) // delivery mode: reliable
            .bytes(&[]); // destination information (unused on a bound TCP link)
    }
    w.finish()
}

async fn read_loop(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    pending: Pending,
    notifications: broadcast::Sender<Notification>,
) {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];

    loop {
        let n = match read_half.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);

        loop {
            let len = match pdu::pdu_len(&buf) {
                Ok(Some(len)) => len,
                Ok(None) => break,
                // A desynchronised stream can't be recovered by guessing; drop
                // the connection and let the caller reconnect.
                Err(_) => return,
            };
            let messages = pdu::decode_pdu(&buf[..len]);
            buf.drain(..len);

            let Ok(messages) = messages else { continue };
            for message in messages {
                match message {
                    Message::Response(r) => {
                        let waiter =
                            pending.lock().expect("pending mutex poisoned").remove(&r.handle);
                        if let Some(waiter) = waiter {
                            let _ = waiter.send(r);
                        }
                    }
                    Message::Notification(n) => {
                        // Fails only when nobody is listening, which is fine.
                        let _ = notifications.send(n);
                    }
                    Message::KeepAlive { .. } | Message::Command(_) => {}
                }
            }
        }
    }

    // Wake everyone still waiting so they fail fast rather than time out.
    pending.lock().expect("pending mutex poisoned").clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::level;
    use crate::pdu::decode_pdu;

    #[test]
    fn subscription_params_target_the_emitters_property_changed_event() {
        let params = subscription_params(Ono(0x1000_8206), true);
        let mut r = Reader::new(&params);
        assert_eq!(r.u32().unwrap(), 0x1000_8206);
        assert_eq!(r.u16().unwrap(), 1); // PropertyChanged def level
        assert_eq!(r.u16().unwrap(), 1); // PropertyChanged index
    }

    #[test]
    fn remove_subscription_omits_the_add_only_fields() {
        assert!(subscription_params(Ono(1), false).len() < subscription_params(Ono(1), true).len());
    }

    /// Stand up a fake device that answers one command, and check the client
    /// correlates the response back to the caller.
    #[tokio::test]
    async fn request_response_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                let Ok(messages) = decode_pdu(&buf[..n]) else { continue };
                for message in messages {
                    if let Message::Command(cmd) = message {
                        // Reply with a float32 gain of -6.0 dB.
                        let mut params = Writer::new();
                        params.f32(-6.0);
                        let response = Response {
                            handle: cmd.handle,
                            status: 0,
                            param_count: 1,
                            params: params.finish(),
                        };
                        socket.write_all(&pdu::encode_response(&response)).await.unwrap();
                    }
                }
            }
        });

        let client = Client::connect(addr).await.unwrap();
        let gain = client.get_f32(Ono(0x1000_8206), level::GAIN).await;
        assert_eq!(gain.unwrap(), -6.0);
    }

    /// Dropping the client must actually hang up, not just stop writing. The
    /// device side sees EOF only if the read task was aborted and the read half
    /// dropped with it — which is the whole point of `Tasks`.
    #[tokio::test]
    async fn dropping_the_client_closes_the_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let device = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            // Read until EOF; returns only once the client has really gone.
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        });

        let client = Client::connect(addr).await.unwrap();
        // A second handle must not, by itself, keep the connection alive.
        let handle = client.set_timeout(Duration::from_millis(50));
        drop(client);
        drop(handle);

        tokio::time::timeout(Duration::from_secs(2), device)
            .await
            .expect("device never saw the connection close")
            .unwrap();
    }

    /// The converse: while a second handle is alive, the connection stays up.
    #[tokio::test]
    async fn a_second_handle_keeps_the_connection_alive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let device = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        });

        let client = Client::connect(addr).await.unwrap();
        let handle = client.set_timeout(Duration::from_millis(50));
        drop(client);

        assert!(
            tokio::time::timeout(Duration::from_millis(300), device).await.is_err(),
            "connection closed while a handle was still held"
        );
        drop(handle);
    }

    #[tokio::test]
    async fn request_times_out_when_the_device_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Hold the connection open and say nothing.
            std::future::pending::<()>().await;
            drop(socket);
        });

        let client = Client::connect(addr).await.unwrap().set_timeout(Duration::from_millis(150));
        let err = client.get_f32(Ono(100), level::GAIN).await.unwrap_err();
        assert!(matches!(err, Error::Timeout));
    }
}

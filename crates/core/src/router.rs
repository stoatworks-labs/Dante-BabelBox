use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use dante_babelbox_oca::{OcaAddress, OcaEvent, OcaValue};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

use crate::local_adapter::LocalAdapter;

/// One directional (or, if `bidirectional`, mutual) link between two
/// specific OCA objects - typically the gain (or mute) object on one
/// vendor's channel and the equivalent object on another vendor's,
/// found and connected by identical role rather than raw channel number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mapping {
    pub from: OcaAddress,
    pub to: OcaAddress,
    #[serde(default)]
    pub bidirectional: bool,
}

type SharedAdapter = Arc<Mutex<Box<dyn LocalAdapter>>>;

/// Fans `OcaEvent`s out to every mapped peer address. Tracks the value it
/// last saw at every address - updated from both incoming events and its
/// own pushes - so it never issues a `set` to a peer that already holds
/// that value. That single rule is what stops a bidirectional mapping
/// looping: a device's own confirmation of a command the Router just sent
/// resolves to "the peer already holds this" and is dropped rather than
/// bounced back to its source. Keying on the value the peer *holds* (not
/// on a one-shot record of the last thing pushed) also contains the
/// adversarial case where two adapters share a broadcast domain - two
/// connections to one physical console, so each hears the other's
/// `NOTIFY`s - which the wire can otherwise report more than once per
/// change; every extra copy still resolves to "already held" and settles
/// in one round. Works uniformly over any device family (preamp control,
/// mic telemetry, or whatever comes next) - every event is just "this
/// object, on this device, now has this value."
pub struct Router {
    devices: RwLock<HashMap<String, SharedAdapter>>,
    listener_handles: RwLock<HashMap<String, tokio::task::AbortHandle>>,
    mappings: RwLock<Vec<Mapping>>,
    last_known: Mutex<HashMap<OcaAddress, OcaValue>>,
}

impl Router {
    pub fn new(mappings: Vec<Mapping>) -> Arc<Self> {
        Arc::new(Self {
            devices: RwLock::new(HashMap::new()),
            listener_handles: RwLock::new(HashMap::new()),
            mappings: RwLock::new(mappings),
            last_known: Mutex::new(HashMap::new()),
        })
    }

    /// Registers a device and starts listening to its event stream
    /// immediately - unlike a one-shot startup batch, this means a
    /// device added while the bridge is already running (e.g. via the
    /// web management API) starts propagating right away, with no
    /// separate "run" step needed. Needs `Arc<Self>` (not just `&self`)
    /// so the spawned listener task can hold its own reference to the
    /// Router. Re-registering an id that's already present replaces it
    /// and aborts the prior listener task first, so a stale one never
    /// keeps running alongside the new one.
    pub async fn register_device(self: &Arc<Self>, id: impl Into<String>, device: SharedAdapter) {
        let id = id.into();
        let rx = device.lock().await.subscribe();
        self.devices.write().unwrap().insert(id.clone(), device);

        let router = Arc::clone(self);
        let task_id = id.clone();
        let handle = tokio::spawn(async move {
            router.listen(task_id, rx).await;
        });

        let old_handle = self.listener_handles.write().unwrap().insert(id, handle.abort_handle());
        if let Some(old) = old_handle {
            old.abort();
        }
    }

    /// Stops listening to a device's events, calls its `disconnect()`,
    /// and removes it from the Router. Returns `true` if a device with
    /// this id was registered. The listener task is aborted explicitly
    /// (rather than relying on the adapter's broadcast channel closing on
    /// its own) so propagation stops deterministically the moment this
    /// returns, not whenever the channel happens to notice.
    pub async fn deregister_device(self: &Arc<Self>, id: &str) -> bool {
        let Some(device) = self.devices.write().unwrap().remove(id) else {
            return false;
        };
        if let Some(handle) = self.listener_handles.write().unwrap().remove(id) {
            handle.abort();
        }
        if let Err(e) = device.lock().await.disconnect().await {
            warn!(device = %id, error = %e, "error disconnecting device");
        }
        true
    }

    /// Replaces the mapping table wholesale, e.g. from a config
    /// hot-reload. For a single addition/removal prefer [`add_mapping`](Self::add_mapping)/
    /// [`remove_mapping`](Self::remove_mapping), which don't risk
    /// clobbering a concurrent edit from another caller.
    pub fn update_mappings(&self, mappings: Vec<Mapping>) {
        *self.mappings.write().unwrap() = mappings;
    }

    /// A snapshot of the current mapping table.
    pub fn mappings(&self) -> Vec<Mapping> {
        self.mappings.read().unwrap().clone()
    }

    pub fn add_mapping(&self, mapping: Mapping) {
        self.mappings.write().unwrap().push(mapping);
    }

    /// Removes the first mapping matching this exact `from`/`to` pair
    /// (direction matters - matches how the pair was originally added).
    /// Returns `true` if a mapping was found and removed.
    pub fn remove_mapping(&self, from: &OcaAddress, to: &OcaAddress) -> bool {
        let mut mappings = self.mappings.write().unwrap();
        let Some(index) = mappings.iter().position(|m| &m.from == from && &m.to == to) else {
            return false;
        };
        mappings.remove(index);
        true
    }

    async fn listen(&self, source_id: String, mut rx: broadcast::Receiver<OcaEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => self.handle_event(event).await,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(device = %source_id, dropped = n, "event receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    async fn handle_event(&self, event: OcaEvent) {
        // Whatever this event reports is now the known value at its
        // address, whether it's a device's own change, a physical move, or
        // a confirmation of a push we made. Recording it for every event
        // is what lets the peer-side check below recognise an already-held
        // value and stop the loop.
        self.last_known.lock().await.insert(event.address.clone(), event.object.value.clone());

        for peer in self.peers_of(&event.address) {
            let device = self.devices.read().unwrap().get(&peer.device_id).cloned();
            let Some(device) = device else {
                warn!(device_id = %peer.device_id, "mapping references unknown device");
                continue;
            };

            // Suppress by value-equality: never push a value the peer
            // already holds. Claiming the value (recording it as held)
            // and the skip decision happen under one lock, before the
            // `set` await, so a concurrent listener - e.g. the peer's own
            // confirming `NOTIFY`, or the same change re-reported by a
            // second adapter on a shared console - sees it as already-held
            // and doesn't bounce it back. This collapses the echo storm to
            // one `set` per direction instead of ten rounds of them.
            {
                let mut known = self.last_known.lock().await;
                if known.get(&peer) == Some(&event.object.value) {
                    debug!(address = ?peer, "peer already holds this value; suppressing echo");
                    continue;
                }
                known.insert(peer.clone(), event.object.value.clone());
            }

            let mut device = device.lock().await;
            if let Err(e) = device.set_object(peer.ono, event.object.value.clone()).await {
                warn!(error = %e, ono = %peer.ono, "failed to propagate object value");
            }
        }
    }

    fn peers_of(&self, addr: &OcaAddress) -> Vec<OcaAddress> {
        let mut peers = Vec::new();
        let mappings = self.mappings.read().unwrap();
        for m in mappings.iter() {
            if &m.from == addr {
                peers.push(m.to.clone());
            } else if m.bidirectional && &m.to == addr {
                peers.push(m.from.clone());
            }
        }
        peers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterError, AdapterResult, DeviceInfo};
    use crate::channel_scheme;
    use async_trait::async_trait;
    use dante_babelbox_oca::{Ono, OcaClass, OcaObject, OcaObjectDescriptor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    struct MockAdapter {
        id: String,
        tx: broadcast::Sender<OcaEvent>,
        state: Arc<StdMutex<HashMap<u32, OcaValue>>>,
        disconnected: Arc<StdMutex<bool>>,
    }

    type MockAdapterParts = (
        MockAdapter,
        broadcast::Sender<OcaEvent>,
        broadcast::Receiver<OcaEvent>,
        Arc<StdMutex<HashMap<u32, OcaValue>>>,
        Arc<StdMutex<bool>>,
    );

    impl MockAdapter {
        /// The returned receiver must be kept alive by the caller: a
        /// `broadcast::Sender::send` errors out once zero receivers remain,
        /// and the Router's own `subscribe()` call (made once its `run()`
        /// task is actually polled) can't be relied on to win that race.
        fn new(id: &str) -> MockAdapterParts {
            let (tx, rx) = broadcast::channel(16);
            let state = Arc::new(StdMutex::new(HashMap::new()));
            let disconnected = Arc::new(StdMutex::new(false));
            (
                Self { id: id.to_string(), tx: tx.clone(), state: state.clone(), disconnected: disconnected.clone() },
                tx,
                rx,
                state,
                disconnected,
            )
        }
    }

    #[async_trait]
    impl LocalAdapter for MockAdapter {
        fn id(&self) -> &str {
            &self.id
        }

        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> AdapterResult<()> {
            *self.disconnected.lock().unwrap() = true;
            Ok(())
        }

        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo { vendor: "mock".into(), model: "mock".into(), address: "127.0.0.1".parse().unwrap() })
        }

        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            Vec::new()
        }

        async fn get_object(&mut self, ono: Ono) -> AdapterResult<OcaValue> {
            self.state.lock().unwrap().get(&ono.0).cloned().ok_or(AdapterError::UnsupportedChannel(0))
        }

        async fn set_object(&mut self, ono: Ono, value: OcaValue) -> AdapterResult<()> {
            self.state.lock().unwrap().insert(ono.0, value);
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            self.tx.subscribe()
        }
    }

    fn gain_event(device_id: &str, ono: u32, gain_db: f32) -> OcaEvent {
        let address = OcaAddress::new(device_id, Ono(ono));
        OcaEvent {
            address: address.clone(),
            object: OcaObject {
                ono: address.ono,
                class: OcaClass::Gain,
                role: "Gain".into(),
                settable: true,
                value: OcaValue::F32(gain_db),
            },
        }
    }

    #[tokio::test]
    async fn propagates_bidirectionally_and_suppresses_echo() {
        let (a, a_tx, _a_rx, a_state, _a_disc) = MockAdapter::new("a");
        let (b, b_tx, _b_rx, b_state, _b_disc) = MockAdapter::new("b");

        let mapping =
            Mapping { from: OcaAddress::new("a", Ono(1)), to: OcaAddress::new("b", Ono(5)), bidirectional: true };

        let router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn LocalAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn LocalAdapter>))).await;

        a_tx.send(gain_event("a", 1, 12.5)).unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&OcaValue::F32(12.5)));

        // 'b' confirms the value the Router just pushed to it - this must
        // be suppressed rather than bounced back to 'a'.
        b_tx.send(gain_event("b", 5, 12.5)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(a_state.lock().unwrap().get(&1).is_none());
    }

    #[tokio::test]
    async fn update_mappings_takes_effect_on_a_running_router() {
        let (a, a_tx, _a_rx, _a_state, _a_disc) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state, _b_disc) = MockAdapter::new("b");
        let (c, _c_tx, _c_rx, c_state, _c_disc) = MockAdapter::new("c");

        let mapping =
            Mapping { from: OcaAddress::new("a", Ono(1)), to: OcaAddress::new("b", Ono(5)), bidirectional: false };

        let router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn LocalAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn LocalAdapter>))).await;
        router.register_device("c", Arc::new(Mutex::new(Box::new(c) as Box<dyn LocalAdapter>))).await;

        a_tx.send(gain_event("a", 1, 1.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&OcaValue::F32(1.0)));
        assert!(c_state.lock().unwrap().get(&9).is_none());

        // Re-point the mapping from b to c entirely, as a config hot-reload would.
        router.update_mappings(vec![Mapping {
            from: OcaAddress::new("a", Ono(1)),
            to: OcaAddress::new("c", Ono(9)),
            bidirectional: false,
        }]);

        a_tx.send(gain_event("a", 1, 2.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(c_state.lock().unwrap().get(&9), Some(&OcaValue::F32(2.0)));
        // b must not have received the post-reload event.
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&OcaValue::F32(1.0)));
    }

    #[tokio::test]
    async fn add_and_remove_mapping_are_single_item_edits() {
        let router = Router::new(vec![Mapping {
            from: OcaAddress::new("a", Ono(1)),
            to: OcaAddress::new("b", Ono(1)),
            bidirectional: true,
        }]);

        router.add_mapping(Mapping {
            from: OcaAddress::new("a", Ono(2)),
            to: OcaAddress::new("b", Ono(2)),
            bidirectional: false,
        });
        assert_eq!(router.mappings().len(), 2);

        let removed = router.remove_mapping(&OcaAddress::new("a", Ono(1)), &OcaAddress::new("b", Ono(1)));
        assert!(removed);
        let remaining = router.mappings();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].from, OcaAddress::new("a", Ono(2)));

        // Removing a pair that was never added, or already removed, is a
        // no-op reported via the return value, not a panic.
        assert!(!router.remove_mapping(&OcaAddress::new("a", Ono(1)), &OcaAddress::new("b", Ono(1))));
    }

    #[tokio::test]
    async fn deregister_device_stops_propagation_and_calls_disconnect() {
        let (a, a_tx, _a_rx, _a_state, _a_disc) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state, b_disc) = MockAdapter::new("b");

        let router = Router::new(vec![Mapping {
            from: OcaAddress::new("a", Ono(1)),
            to: OcaAddress::new("b", Ono(1)),
            bidirectional: false,
        }]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn LocalAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn LocalAdapter>))).await;

        let removed = router.deregister_device("b").await;
        assert!(removed);
        assert!(*b_disc.lock().unwrap(), "disconnect() must be called on removal");

        a_tx.send(gain_event("a", 1, 1.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(b_state.lock().unwrap().is_empty(), "removed device must not receive further events");

        assert!(!router.deregister_device("b").await, "removing an already-removed device is a no-op");
    }

    #[tokio::test]
    async fn re_registering_a_device_id_replaces_the_old_listener() {
        let (a1, a1_tx, _a1_rx, _a1_state, _a1_disc) = MockAdapter::new("a");
        let (a2, a2_tx, _a2_rx, _a2_state, _a2_disc) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state, _b_disc) = MockAdapter::new("b");

        let router = Router::new(vec![Mapping {
            from: OcaAddress::new("a", Ono(1)),
            to: OcaAddress::new("b", Ono(1)),
            bidirectional: false,
        }]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a1) as Box<dyn LocalAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn LocalAdapter>))).await;
        // Re-register the same id with a new adapter instance, as a live
        // "add device" via the web API replacing a stale one would.
        router.register_device("a", Arc::new(Mutex::new(Box::new(a2) as Box<dyn LocalAdapter>))).await;

        // The old instance's sender should have no effect (its listener
        // task was aborted); only the new instance's events propagate.
        let _ = a1_tx.send(gain_event("a", 1, 9.0));
        a2_tx.send(gain_event("a", 1, 5.0)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(b_state.lock().unwrap().get(&1), Some(&OcaValue::F32(5.0)));
    }

    /// Models the adversarial bench topology of §7c: two `yamaha-dm3-scp`
    /// adapters (`dm3-a`, `dm3-b`) that are really two TCP connections to
    /// **one** physical console, bidirectionally mapped `ch11 <-> ch12`.
    ///
    /// The defeater a single clean connection doesn't have is *multiplicity*:
    /// the shared desk reports one write more than once - its reply to the
    /// connection that wrote, plus the broadcast `NOTIFY` every connection
    /// hears - so the Router sees the same confirmation twice. `report_copies`
    /// is that fan-out (1 = a lone clean link, which never bounced;
    /// `>= 2` = the shared desk, which did). A suppression that consumed a
    /// one-shot record damped only the first copy and bounced the rest; the
    /// value-equality rule drops every copy of an already-held value.
    struct SharedConsole {
        report_copies: usize,
        emissions: AtomicUsize,
        emit_cap: usize,
        /// Every `(device_id, value)` the Router actually pushed - its
        /// length is the `set`-command count the test asserts on.
        applied: StdMutex<Vec<(String, f32)>>,
    }

    impl SharedConsole {
        /// Re-report a write's new value on the writer's own stream, as many
        /// times as the shared desk would surface it, bounded by `emit_cap`
        /// so a regressed (looping) Router can't spin forever in the test.
        fn report(&self, tx: &broadcast::Sender<OcaEvent>, address: &OcaAddress, gain_db: f32) {
            for _ in 0..self.report_copies {
                if self.emissions.fetch_add(1, Ordering::SeqCst) >= self.emit_cap {
                    return;
                }
                let _ = tx.send(gain_event_at(address, gain_db));
            }
        }
    }

    fn gain_event_at(address: &OcaAddress, gain_db: f32) -> OcaEvent {
        OcaEvent {
            address: address.clone(),
            object: OcaObject {
                ono: address.ono,
                class: OcaClass::Gain,
                role: "Gain".into(),
                settable: true,
                value: OcaValue::F32(gain_db),
            },
        }
    }

    struct SharedConsoleAdapter {
        device_id: String,
        /// This connection's view of its mapped object (its own `device_id`
        /// with the ono of the channel it fronts).
        endpoint: OcaAddress,
        tx: broadcast::Sender<OcaEvent>,
        console: Arc<SharedConsole>,
    }

    #[async_trait]
    impl LocalAdapter for SharedConsoleAdapter {
        fn id(&self) -> &str {
            &self.device_id
        }

        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo { vendor: "mock".into(), model: "shared-console".into(), address: "127.0.0.1".parse().unwrap() })
        }

        fn describe(&self) -> Vec<OcaObjectDescriptor> {
            Vec::new()
        }

        async fn get_object(&mut self, _ono: Ono) -> AdapterResult<OcaValue> {
            Err(AdapterError::UnsupportedChannel(0))
        }

        async fn set_object(&mut self, _ono: Ono, value: OcaValue) -> AdapterResult<()> {
            let gain = value.as_f32().expect("bench mapping carries gain as f32");
            self.console.applied.lock().unwrap().push((self.device_id.clone(), gain));
            // The write lands on the shared desk, which re-broadcasts the
            // new value to every connection - the adversarial echo the
            // Router must damp in one round rather than bounce.
            self.console.report(&self.tx, &self.endpoint, gain);
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
            self.tx.subscribe()
        }
    }

    #[tokio::test]
    async fn shared_broadcast_domain_converges_in_one_round_instead_of_amplifying() {
        // dm3-a fronts ch11, dm3-b fronts ch12, both on the same desk.
        let (a_tx, _a_rx0) = broadcast::channel(64);
        let (b_tx, _b_rx0) = broadcast::channel(64);
        let a_addr = OcaAddress::new("dm3-a", channel_scheme::gain_ono(11));
        let b_addr = OcaAddress::new("dm3-b", channel_scheme::gain_ono(12));

        let console = Arc::new(SharedConsole {
            // Two copies per write == the shared desk (reply + broadcast).
            // This is exactly the case that bounced ~42 sets over ~10 rounds
            // on the bench; with `report_copies` at 1 the old Router already
            // converged, which is why two *distinct* devices never looped.
            report_copies: 2,
            emissions: AtomicUsize::new(0),
            emit_cap: 64,
            applied: StdMutex::new(Vec::new()),
        });

        let adapter_a = SharedConsoleAdapter {
            device_id: "dm3-a".into(),
            endpoint: a_addr.clone(),
            tx: a_tx.clone(),
            console: console.clone(),
        };
        let adapter_b = SharedConsoleAdapter {
            device_id: "dm3-b".into(),
            endpoint: b_addr.clone(),
            tx: b_tx.clone(),
            console: console.clone(),
        };

        let router = Router::new(vec![Mapping { from: a_addr.clone(), to: b_addr.clone(), bidirectional: true }]);
        router.register_device("dm3-a", Arc::new(Mutex::new(Box::new(adapter_a) as Box<dyn LocalAdapter>))).await;
        router.register_device("dm3-b", Arc::new(Mutex::new(Box::new(adapter_b) as Box<dyn LocalAdapter>))).await;

        // User action 1: move ch11 to 22. Its single NOTIFY reaches dm3-a's
        // stream; the Router should pull ch12 to match in one write.
        let _ = a_tx.send(gain_event_at(&a_addr, 22.0));
        tokio::time::sleep(Duration::from_millis(150)).await;

        {
            let applied = console.applied.lock().unwrap();
            assert!(
                applied.iter().any(|(d, v)| d == "dm3-b" && *v == 22.0),
                "ch11->ch12 never propagated: {applied:?}",
            );
            // The core assertion: ~1 set per direction, not ~10. Before the
            // value-equality rule this ran away to `emit_cap`.
            assert!(
                applied.len() <= 2,
                "echo storm not contained: {} set commands for one user action \
                 (want <= 2, ~1 per direction): {applied:?}",
                applied.len(),
            );
        }

        // User action 2: move ch12 to 33 - the other direction, and a value
        // neither side holds, so a legitimate propagation that must still go
        // through (the fix suppresses re-sends of already-held values, never
        // genuine changes).
        console.applied.lock().unwrap().clear();
        console.emissions.store(0, Ordering::SeqCst);
        let _ = b_tx.send(gain_event_at(&b_addr, 33.0));
        tokio::time::sleep(Duration::from_millis(150)).await;

        let applied = console.applied.lock().unwrap();
        assert!(
            applied.iter().any(|(d, v)| d == "dm3-a" && *v == 33.0),
            "ch12->ch11 never propagated - suppression is eating a legitimate change: {applied:?}",
        );
        assert!(
            applied.len() <= 2,
            "echo storm not contained on the reverse direction: {} set commands \
             for one user action (want <= 2): {applied:?}",
            applied.len(),
        );
    }
}

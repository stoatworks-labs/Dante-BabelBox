use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

use crate::adapter::DeviceAdapter;
use crate::types::{PreampAddress, PreampEvent, PreampState};

/// One directional (or, if `bidirectional`, mutual) link between two
/// physical preamp channels, typically on different vendors' gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mapping {
    pub from: PreampAddress,
    pub to: PreampAddress,
    #[serde(default)]
    pub bidirectional: bool,
}

type SharedAdapter = Arc<Mutex<Box<dyn DeviceAdapter>>>;

/// Fans PreampEvents out to every mapped peer address. Tracks the last
/// state it pushed to each address so a device's own confirmation of a
/// command the Router just sent isn't mistaken for an independent change
/// and bounced back to its source (which would otherwise loop forever
/// between two bidirectionally-mapped devices).
pub struct Router {
    devices: RwLock<HashMap<String, SharedAdapter>>,
    listener_handles: RwLock<HashMap<String, tokio::task::AbortHandle>>,
    mappings: RwLock<Vec<Mapping>>,
    last_pushed: Mutex<HashMap<PreampAddress, PreampState>>,
}

impl Router {
    pub fn new(mappings: Vec<Mapping>) -> Arc<Self> {
        Arc::new(Self {
            devices: RwLock::new(HashMap::new()),
            listener_handles: RwLock::new(HashMap::new()),
            mappings: RwLock::new(mappings),
            last_pushed: Mutex::new(HashMap::new()),
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
    pub fn remove_mapping(&self, from: &PreampAddress, to: &PreampAddress) -> bool {
        let mut mappings = self.mappings.write().unwrap();
        let Some(index) = mappings.iter().position(|m| &m.from == from && &m.to == to) else {
            return false;
        };
        mappings.remove(index);
        true
    }

    async fn listen(&self, source_id: String, mut rx: broadcast::Receiver<PreampEvent>) {
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

    async fn handle_event(&self, event: PreampEvent) {
        if self.is_echo(&event).await {
            debug!(address = ?event.address, "suppressing echo of our own push");
            return;
        }

        for peer in self.peers_of(&event.address) {
            let device = self.devices.read().unwrap().get(&peer.device_id).cloned();
            let Some(device) = device else {
                warn!(device_id = %peer.device_id, "mapping references unknown device");
                continue;
            };
            self.record_push(peer.clone(), event.state).await;

            let mut device = device.lock().await;
            if let Err(e) = device.set_gain(peer.channel, event.state.gain_db).await {
                warn!(error = %e, channel = peer.channel, "failed to propagate gain");
            }
            if let Err(e) = device.set_phantom(peer.channel, event.state.phantom).await {
                warn!(error = %e, channel = peer.channel, "failed to propagate phantom");
            }
        }
    }

    fn peers_of(&self, addr: &PreampAddress) -> Vec<PreampAddress> {
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

    async fn is_echo(&self, event: &PreampEvent) -> bool {
        let mut last_pushed = self.last_pushed.lock().await;
        if last_pushed.get(&event.address) == Some(&event.state) {
            last_pushed.remove(&event.address);
            true
        } else {
            false
        }
    }

    async fn record_push(&self, address: PreampAddress, state: PreampState) {
        self.last_pushed.lock().await.insert(address, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterError, AdapterResult, DeviceInfo};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    struct MockAdapter {
        id: String,
        tx: broadcast::Sender<PreampEvent>,
        state: Arc<StdMutex<HashMap<u16, PreampState>>>,
        disconnected: Arc<StdMutex<bool>>,
    }

    type MockAdapterParts = (
        MockAdapter,
        broadcast::Sender<PreampEvent>,
        broadcast::Receiver<PreampEvent>,
        Arc<StdMutex<HashMap<u16, PreampState>>>,
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
                Self {
                    id: id.to_string(),
                    tx: tx.clone(),
                    state: state.clone(),
                    disconnected: disconnected.clone(),
                },
                tx,
                rx,
                state,
                disconnected,
            )
        }
    }

    #[async_trait]
    impl DeviceAdapter for MockAdapter {
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
            Ok(DeviceInfo {
                vendor: "mock".into(),
                model: "mock".into(),
                address: "127.0.0.1".parse().unwrap(),
            })
        }

        async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
            let mut s = self.state.lock().unwrap();
            let entry = s.entry(channel).or_insert(PreampState {
                gain_db: 0.0,
                phantom: false,
                pad: None,
            });
            entry.gain_db = gain_db;
            Ok(())
        }

        async fn set_phantom(&mut self, channel: u16, on: bool) -> AdapterResult<()> {
            let mut s = self.state.lock().unwrap();
            let entry = s.entry(channel).or_insert(PreampState {
                gain_db: 0.0,
                phantom: false,
                pad: None,
            });
            entry.phantom = on;
            Ok(())
        }

        async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
            self.state
                .lock()
                .unwrap()
                .get(&channel)
                .copied()
                .ok_or(AdapterError::UnsupportedChannel(channel))
        }

        fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
            self.tx.subscribe()
        }
    }

    #[tokio::test]
    async fn propagates_bidirectionally_and_suppresses_echo() {
        let (a, a_tx, _a_rx, a_state, _a_disc) = MockAdapter::new("a");
        let (b, b_tx, _b_rx, b_state, _b_disc) = MockAdapter::new("b");

        let mapping = Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 5),
            bidirectional: true,
        };

        let router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn DeviceAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>))).await;

        let state = PreampState {
            gain_db: 12.5,
            phantom: true,
            pad: None,
        };
        a_tx.send(PreampEvent {
            address: PreampAddress::new("a", 1),
            state,
        })
        .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&state));

        // 'b' confirms the value the Router just pushed to it - this must
        // be suppressed rather than bounced back to 'a'.
        b_tx.send(PreampEvent {
            address: PreampAddress::new("b", 5),
            state,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(a_state.lock().unwrap().get(&1).is_none());
    }

    #[tokio::test]
    async fn update_mappings_takes_effect_on_a_running_router() {
        let (a, a_tx, _a_rx, _a_state, _a_disc) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state, _b_disc) = MockAdapter::new("b");
        let (c, _c_tx, _c_rx, c_state, _c_disc) = MockAdapter::new("c");

        let mapping = Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 5),
            bidirectional: false,
        };

        let router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn DeviceAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>))).await;
        router.register_device("c", Arc::new(Mutex::new(Box::new(c) as Box<dyn DeviceAdapter>))).await;

        let state = PreampState {
            gain_db: 1.0,
            phantom: false,
            pad: None,
        };
        a_tx.send(PreampEvent {
            address: PreampAddress::new("a", 1),
            state,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&state));
        assert!(c_state.lock().unwrap().get(&9).is_none());

        // Re-point the mapping from b to c entirely, as a config hot-reload would.
        router.update_mappings(vec![Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("c", 9),
            bidirectional: false,
        }]);

        let state2 = PreampState {
            gain_db: 2.0,
            phantom: true,
            pad: None,
        };
        a_tx.send(PreampEvent {
            address: PreampAddress::new("a", 1),
            state: state2,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(c_state.lock().unwrap().get(&9), Some(&state2));
        // b must not have received the post-reload event.
        assert_eq!(b_state.lock().unwrap().get(&5), Some(&state));
    }

    #[tokio::test]
    async fn add_and_remove_mapping_are_single_item_edits() {
        let router = Router::new(vec![Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 1),
            bidirectional: true,
        }]);

        router.add_mapping(Mapping {
            from: PreampAddress::new("a", 2),
            to: PreampAddress::new("b", 2),
            bidirectional: false,
        });
        assert_eq!(router.mappings().len(), 2);

        let removed = router.remove_mapping(&PreampAddress::new("a", 1), &PreampAddress::new("b", 1));
        assert!(removed);
        let remaining = router.mappings();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].from, PreampAddress::new("a", 2));

        // Removing a pair that was never added, or already removed, is a
        // no-op reported via the return value, not a panic.
        assert!(!router.remove_mapping(&PreampAddress::new("a", 1), &PreampAddress::new("b", 1)));
    }

    #[tokio::test]
    async fn deregister_device_stops_propagation_and_calls_disconnect() {
        let (a, a_tx, _a_rx, _a_state, _a_disc) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state, b_disc) = MockAdapter::new("b");

        let router = Router::new(vec![Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 1),
            bidirectional: false,
        }]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn DeviceAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>))).await;

        let removed = router.deregister_device("b").await;
        assert!(removed);
        assert!(*b_disc.lock().unwrap(), "disconnect() must be called on removal");

        a_tx.send(PreampEvent {
            address: PreampAddress::new("a", 1),
            state: PreampState { gain_db: 1.0, phantom: false, pad: None },
        })
        .unwrap();
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
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 1),
            bidirectional: false,
        }]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a1) as Box<dyn DeviceAdapter>))).await;
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>))).await;
        // Re-register the same id with a new adapter instance, as a live
        // "add device" via the web API replacing a stale one would.
        router.register_device("a", Arc::new(Mutex::new(Box::new(a2) as Box<dyn DeviceAdapter>))).await;

        // The old instance's sender should have no effect (its listener
        // task was aborted); only the new instance's events propagate.
        let _ = a1_tx.send(PreampEvent {
            address: PreampAddress::new("a", 1),
            state: PreampState { gain_db: 9.0, phantom: false, pad: None },
        });
        a2_tx
            .send(PreampEvent {
                address: PreampAddress::new("a", 1),
                state: PreampState { gain_db: 5.0, phantom: true, pad: None },
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            b_state.lock().unwrap().get(&1),
            Some(&PreampState { gain_db: 5.0, phantom: true, pad: None })
        );
    }
}

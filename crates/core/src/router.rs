use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Deserialize;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

use crate::adapter::DeviceAdapter;
use crate::types::{PreampAddress, PreampEvent, PreampState};

/// One directional (or, if `bidirectional`, mutual) link between two
/// physical preamp channels, typically on different vendors' gear.
#[derive(Debug, Clone, Deserialize)]
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
    devices: HashMap<String, SharedAdapter>,
    mappings: RwLock<Vec<Mapping>>,
    last_pushed: Mutex<HashMap<PreampAddress, PreampState>>,
}

impl Router {
    pub fn new(mappings: Vec<Mapping>) -> Self {
        Self {
            devices: HashMap::new(),
            mappings: RwLock::new(mappings),
            last_pushed: Mutex::new(HashMap::new()),
        }
    }

    pub fn register_device(&mut self, id: impl Into<String>, device: SharedAdapter) {
        self.devices.insert(id.into(), device);
    }

    /// Replaces the mapping table live, e.g. from a config hot-reload.
    /// Devices themselves aren't affected - adding/removing a device
    /// still requires a restart, only which channels are linked can
    /// change without dropping live connections.
    pub fn update_mappings(&self, mappings: Vec<Mapping>) {
        *self.mappings.write().unwrap() = mappings;
    }

    /// Runs until every device's event stream closes. Spawns one listener
    /// task per device and awaits them all.
    pub async fn run(self: Arc<Self>) {
        let mut handles = Vec::new();
        for (id, device) in self.devices.iter() {
            let rx = device.lock().await.subscribe();
            let router = Arc::clone(&self);
            let id = id.clone();
            handles.push(tokio::spawn(async move {
                router.listen(id, rx).await;
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
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
            let Some(device) = self.devices.get(&peer.device_id).cloned() else {
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
    }

    type MockAdapterParts = (
        MockAdapter,
        broadcast::Sender<PreampEvent>,
        broadcast::Receiver<PreampEvent>,
        Arc<StdMutex<HashMap<u16, PreampState>>>,
    );

    impl MockAdapter {
        /// The returned receiver must be kept alive by the caller: a
        /// `broadcast::Sender::send` errors out once zero receivers remain,
        /// and the Router's own `subscribe()` call (made once its `run()`
        /// task is actually polled) can't be relied on to win that race.
        fn new(id: &str) -> MockAdapterParts {
            let (tx, rx) = broadcast::channel(16);
            let state = Arc::new(StdMutex::new(HashMap::new()));
            (
                Self {
                    id: id.to_string(),
                    tx: tx.clone(),
                    state: state.clone(),
                },
                tx,
                rx,
                state,
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
        let (a, a_tx, _a_rx, a_state) = MockAdapter::new("a");
        let (b, b_tx, _b_rx, b_state) = MockAdapter::new("b");

        let mapping = Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 5),
            bidirectional: true,
        };

        let mut router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn DeviceAdapter>)));
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>)));
        let router = Arc::new(router);

        let router_task = tokio::spawn(Arc::clone(&router).run());
        // Let the router task run far enough to call subscribe() on each
        // device before we publish - a broadcast receiver only sees events
        // sent after it subscribed.
        tokio::time::sleep(Duration::from_millis(20)).await;

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

        router_task.abort();
    }

    #[tokio::test]
    async fn update_mappings_takes_effect_on_a_running_router() {
        let (a, a_tx, _a_rx, _a_state) = MockAdapter::new("a");
        let (b, _b_tx, _b_rx, b_state) = MockAdapter::new("b");
        let (c, _c_tx, _c_rx, c_state) = MockAdapter::new("c");

        let mapping = Mapping {
            from: PreampAddress::new("a", 1),
            to: PreampAddress::new("b", 5),
            bidirectional: false,
        };

        let mut router = Router::new(vec![mapping]);
        router.register_device("a", Arc::new(Mutex::new(Box::new(a) as Box<dyn DeviceAdapter>)));
        router.register_device("b", Arc::new(Mutex::new(Box::new(b) as Box<dyn DeviceAdapter>)));
        router.register_device("c", Arc::new(Mutex::new(Box::new(c) as Box<dyn DeviceAdapter>)));
        let router = Arc::new(router);

        let router_task = tokio::spawn(Arc::clone(&router).run());
        tokio::time::sleep(Duration::from_millis(20)).await;

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

        router_task.abort();
    }
}

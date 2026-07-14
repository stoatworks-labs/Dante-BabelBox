mod adapter;
mod types;

pub use adapter::MicAdapter;
pub use dante_babelbox_core::{AdapterError, AdapterResult, DeviceInfo};
pub use types::{AntennaDiversity, MicAddress, MicEvent, MicState};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use tokio::sync::Mutex as AsyncMutex;

    /// Hand-rolled mock mirroring `dante_babelbox_core::router`'s
    /// `MockAdapter` shape: a `broadcast::Sender` to simulate
    /// device-originated telemetry, and a captured-calls map so tests can
    /// assert on what `set_mute` was actually called with.
    struct MockAdapter {
        id: String,
        tx: broadcast::Sender<MicEvent>,
        mute_calls: Arc<AsyncMutex<HashMap<u16, bool>>>,
    }

    impl MockAdapter {
        fn new(id: &str) -> (Self, broadcast::Sender<MicEvent>, Arc<AsyncMutex<HashMap<u16, bool>>>) {
            let (tx, _rx) = broadcast::channel(16);
            let mute_calls = Arc::new(AsyncMutex::new(HashMap::new()));
            (
                Self {
                    id: id.to_string(),
                    tx: tx.clone(),
                    mute_calls: mute_calls.clone(),
                },
                tx,
                mute_calls,
            )
        }
    }

    #[async_trait::async_trait]
    impl MicAdapter for MockAdapter {
        fn id(&self) -> &str {
            &self.id
        }

        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo {
                vendor: "Mock".into(),
                model: "MockMic".into(),
                address: "127.0.0.1".parse().unwrap(),
            })
        }

        async fn get_state(&mut self, _channel: u16) -> AdapterResult<MicState> {
            Ok(MicState {
                battery_percent: Some(80),
                battery_minutes_remaining: Some(300),
                rf_level_dbm: Some(-45),
                audio_level: Some(20),
                muted: false,
                frequency_mhz: Some(614.125),
                antenna: Some(AntennaDiversity::A),
            })
        }

        async fn set_mute(&mut self, channel: u16, muted: bool) -> AdapterResult<()> {
            self.mute_calls.lock().await.insert(channel, muted);
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<MicEvent> {
            self.tx.subscribe()
        }
    }

    #[tokio::test]
    async fn set_mute_is_recorded_and_events_are_observable() {
        let (mut adapter, tx, mute_calls) = MockAdapter::new("ulxd-1");

        // Subscribe before publishing - broadcast::Sender::send errors
        // with zero receivers, so this must happen first.
        let mut rx = adapter.subscribe();

        adapter.set_mute(2, true).await.unwrap();
        assert_eq!(mute_calls.lock().await.get(&2), Some(&true));

        let event = MicEvent {
            address: MicAddress::new("ulxd-1", 2),
            state: MicState {
                battery_percent: Some(55),
                battery_minutes_remaining: Some(120),
                rf_level_dbm: Some(-52),
                audio_level: Some(12),
                muted: true,
                frequency_mhz: Some(614.125),
                antenna: Some(AntennaDiversity::B),
            },
        };
        tx.send(event.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.address, event.address);
        assert_eq!(received.state, event.state);
    }
}

//! Bridges the old preamp-specific [`DeviceAdapter`] trait (still
//! implemented directly by the vendor adapters not yet converted to real
//! `cdylib` plugins - Wing, AHM, dLive, DM3) into the generic
//! [`LocalAdapter`]/OCA shape the [`crate::Router`] now speaks. One
//! generic shim covers all four, since they already share an identical
//! trait surface - there's nothing vendor-specific left to write per
//! adapter here.
//!
//! Each channel gets three OCA objects, addressed via
//! [`crate::channel_scheme`]'s shared per-channel Ono convention - the same
//! one virtual devices use to synthesize a descriptor set with no live
//! adapter at all, so a config-file/API mapping specified by device +
//! channel number resolves identically whether either side is a real
//! (legacy-shimmed) device or a virtual placeholder. This is *that
//! scheme's own* convention for adapters that were never OCA-native to
//! begin with - unrelated to any real device's actual (if it has one) ONo
//! scheme, and not wire-exposed.

use async_trait::async_trait;
use dante_babelbox_oca::{Ono, OcaAddress, OcaEvent, OcaObject, OcaObjectDescriptor, OcaValue};
use tokio::sync::broadcast;

use crate::adapter::{AdapterError, AdapterResult, DeviceAdapter, DeviceInfo};
use crate::channel_scheme::{self, Field};
use crate::local_adapter::LocalAdapter;
use crate::types::PreampEvent;

fn preamp_event_to_oca(device_id: &str, event: PreampEvent) -> Vec<OcaEvent> {
    let channel = event.address.channel;
    let mut out = vec![
        OcaEvent {
            address: OcaAddress::new(device_id, channel_scheme::gain_ono(channel)),
            object: OcaObject::from_descriptor(
                channel_scheme::descriptor(channel, Field::Gain),
                OcaValue::F32(event.state.gain_db),
            ),
        },
        OcaEvent {
            address: OcaAddress::new(device_id, channel_scheme::phantom_ono(channel)),
            object: OcaObject::from_descriptor(
                channel_scheme::descriptor(channel, Field::Phantom),
                OcaValue::Bool(event.state.phantom),
            ),
        },
    ];
    if let Some(pad) = event.state.pad {
        out.push(OcaEvent {
            address: OcaAddress::new(device_id, channel_scheme::pad_ono(channel)),
            object: OcaObject::from_descriptor(channel_scheme::descriptor(channel, Field::Pad), OcaValue::Bool(pad)),
        });
    }
    out
}

/// Wraps any `Box<dyn DeviceAdapter>` (Wing, AHM, dLive, DM3 today) as a
/// [`LocalAdapter`]. `channels` must match the device's actual channel
/// count (from `DeviceConfig::channel_count()`) so `describe()` and Ono
/// decoding cover exactly the range the device really has.
pub struct LegacyPreampShim {
    inner: Box<dyn DeviceAdapter>,
    channels: u16,
    event_tx: broadcast::Sender<OcaEvent>,
}

impl LegacyPreampShim {
    pub fn new(inner: Box<dyn DeviceAdapter>, channels: u16) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        let mut inner_rx = inner.subscribe();
        let device_id = inner.id().to_string();
        let tx = event_tx.clone();

        tokio::spawn(async move {
            loop {
                match inner_rx.recv().await {
                    Ok(event) => {
                        for oca_event in preamp_event_to_oca(&device_id, event) {
                            let _ = tx.send(oca_event);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self { inner, channels, event_tx }
    }
}

#[async_trait]
impl LocalAdapter for LegacyPreampShim {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        self.inner.connect().await
    }

    async fn disconnect(&mut self) -> AdapterResult<()> {
        self.inner.disconnect().await
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        self.inner.identify().await
    }

    fn describe(&self) -> Vec<OcaObjectDescriptor> {
        channel_scheme::descriptors_for_channels(self.channels)
    }

    async fn get_object(&mut self, ono: Ono) -> AdapterResult<OcaValue> {
        let (channel, field) = channel_scheme::decode_ono(ono, self.channels)
            .ok_or_else(|| AdapterError::Protocol(format!("no such object {ono}")))?;
        let state = self.inner.get_state(channel).await?;
        match field {
            Field::Gain => Ok(OcaValue::F32(state.gain_db)),
            Field::Phantom => Ok(OcaValue::Bool(state.phantom)),
            Field::Pad => state
                .pad
                .map(OcaValue::Bool)
                .ok_or_else(|| AdapterError::Protocol(format!("channel {channel} has no pad switch"))),
        }
    }

    async fn set_object(&mut self, ono: Ono, value: OcaValue) -> AdapterResult<()> {
        let (channel, field) = channel_scheme::decode_ono(ono, self.channels)
            .ok_or_else(|| AdapterError::Protocol(format!("no such object {ono}")))?;
        match field {
            Field::Gain => {
                let v = value
                    .as_f32()
                    .ok_or_else(|| AdapterError::Protocol("gain requires an F32 value".into()))?;
                self.inner.set_gain(channel, v).await
            }
            Field::Phantom => {
                let v = value
                    .as_bool()
                    .ok_or_else(|| AdapterError::Protocol("phantom requires a Bool value".into()))?;
                self.inner.set_phantom(channel, v).await
            }
            Field::Pad => Err(AdapterError::Protocol(format!("channel {channel}'s pad switch is read-only"))),
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<OcaEvent> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PreampAddress;
    use crate::types::PreampState;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    struct MockDeviceAdapter {
        id: String,
        tx: broadcast::Sender<PreampEvent>,
        state: Arc<StdMutex<HashMap<u16, PreampState>>>,
        gain_calls: Arc<StdMutex<Vec<(u16, f32)>>>,
    }

    #[async_trait]
    impl DeviceAdapter for MockDeviceAdapter {
        fn id(&self) -> &str {
            &self.id
        }

        async fn connect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> AdapterResult<()> {
            Ok(())
        }

        async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
            Ok(DeviceInfo { vendor: "mock".into(), model: "mock".into(), address: "127.0.0.1".parse().unwrap() })
        }

        async fn set_gain(&mut self, channel: u16, gain_db: f32) -> AdapterResult<()> {
            self.gain_calls.lock().unwrap().push((channel, gain_db));
            Ok(())
        }

        async fn set_phantom(&mut self, _channel: u16, _on: bool) -> AdapterResult<()> {
            Ok(())
        }

        async fn get_state(&mut self, channel: u16) -> AdapterResult<PreampState> {
            self.state.lock().unwrap().get(&channel).copied().ok_or(AdapterError::UnsupportedChannel(channel))
        }

        fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
            self.tx.subscribe()
        }
    }

    // Ono encode/decode round-tripping is covered by
    // `channel_scheme`'s own tests - nothing shim-specific to add here.

    #[tokio::test]
    async fn describe_covers_every_channel_with_three_objects_each() {
        let (_tx, _rx) = broadcast::channel::<PreampEvent>(1);
        let adapter = MockDeviceAdapter {
            id: "d".into(),
            tx: _tx,
            state: Arc::new(StdMutex::new(HashMap::new())),
            gain_calls: Arc::new(StdMutex::new(Vec::new())),
        };
        let shim = LegacyPreampShim::new(Box::new(adapter), 4);
        assert_eq!(shim.describe().len(), 12);
        assert!(shim.describe().iter().any(|d| d.role == "Ch 2 Gain" && d.settable));
        assert!(shim.describe().iter().any(|d| d.role == "Ch 4 Pad" && !d.settable));
    }

    #[tokio::test]
    async fn get_and_set_object_translate_through_channel_numbers() {
        let (tx, _rx) = broadcast::channel::<PreampEvent>(1);
        let state = Arc::new(StdMutex::new(HashMap::new()));
        state.lock().unwrap().insert(2, PreampState { gain_db: -3.0, phantom: true, pad: Some(false) });
        let gain_calls = Arc::new(StdMutex::new(Vec::new()));

        let adapter = MockDeviceAdapter { id: "d".into(), tx, state: state.clone(), gain_calls: gain_calls.clone() };
        let mut shim = LegacyPreampShim::new(Box::new(adapter), 4);

        assert_eq!(shim.get_object(channel_scheme::gain_ono(2)).await.unwrap(), OcaValue::F32(-3.0));
        assert_eq!(shim.get_object(channel_scheme::phantom_ono(2)).await.unwrap(), OcaValue::Bool(true));
        assert_eq!(shim.get_object(channel_scheme::pad_ono(2)).await.unwrap(), OcaValue::Bool(false));

        shim.set_object(channel_scheme::gain_ono(2), OcaValue::F32(6.0)).await.unwrap();
        assert_eq!(*gain_calls.lock().unwrap(), vec![(2, 6.0)]);

        let err = shim.set_object(channel_scheme::pad_ono(2), OcaValue::Bool(true)).await.unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
    }

    #[tokio::test]
    async fn subscribe_translates_one_preamp_event_into_gain_and_phantom_oca_events() {
        let (tx, _rx) = broadcast::channel::<PreampEvent>(4);
        let adapter = MockDeviceAdapter {
            id: "d".into(),
            tx: tx.clone(),
            state: Arc::new(StdMutex::new(HashMap::new())),
            gain_calls: Arc::new(StdMutex::new(Vec::new())),
        };
        let shim = LegacyPreampShim::new(Box::new(adapter), 4);
        let mut oca_rx = shim.subscribe();

        tx.send(PreampEvent {
            address: PreampAddress::new("d", 3),
            state: PreampState { gain_db: 1.5, phantom: false, pad: None },
        })
        .unwrap();

        let first = tokio::time::timeout(Duration::from_millis(200), oca_rx.recv()).await.unwrap().unwrap();
        assert_eq!(first.object.value, OcaValue::F32(1.5));
        let second = tokio::time::timeout(Duration::from_millis(200), oca_rx.recv()).await.unwrap().unwrap();
        assert_eq!(second.object.value, OcaValue::Bool(false));
        // pad was None, so only two events should have been emitted.
        assert!(tokio::time::timeout(Duration::from_millis(50), oca_rx.recv()).await.is_err());
    }
}

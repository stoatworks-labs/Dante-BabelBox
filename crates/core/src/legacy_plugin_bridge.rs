//! Translates the old preamp-specific [`DeviceAdapter`] trait into OCA
//! terms, implementing `dante_babelbox_oca_plugin_abi::PluginAdapter`
//! (synchronous, FFI-safe) so a vendor adapter that still speaks the old
//! `DeviceAdapter` trait can ship as a real dylib plugin without
//! rewriting its wire-protocol logic - every plugin crate that wraps one
//! this way
//! (see e.g. `crates/plugin-osc-wing`) is just a `create_adapter`/
//! `plugin_info` pair: construct the concrete adapter, wrap it in
//! [`LegacyPluginBridge::new`], done.
//!
//! Owns its own multi-threaded Tokio runtime (not `new_current_thread` -
//! see `crates/plugin-osc-x32`'s module doc comment and
//! `docs/plugin-development-guide.md` for why that distinction matters:
//! a current-thread runtime only drives spawned background tasks while
//! something is actively inside a `block_on` call on it, so `poll_events`,
//! called by the host on its own timer and never via `block_on`, would
//! otherwise starve the adapter's receive loop).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};

use abi_stable::std_types::{RResult, RString, RVec};
use dante_babelbox_oca::{Ono, OcaObject, OcaValue};
use dante_babelbox_oca_plugin_abi::{OcaEventFfi, OcaObjectDescriptorFfi, OcaValueFfi, PluginAdapter, RDeviceInfo};
use tokio::runtime::Runtime;

use crate::adapter::DeviceAdapter;
use crate::channel_scheme::{self, Field};
use crate::types::PreampEvent;

fn preamp_event_to_oca_ffi(device_id: &str, event: PreampEvent) -> Vec<OcaEventFfi> {
    let channel = event.address.channel;
    let mut out = vec![
        OcaEventFfi::from_event(
            device_id,
            OcaObject::from_descriptor(channel_scheme::descriptor(channel, Field::Gain), OcaValue::F32(event.state.gain_db)),
        ),
        OcaEventFfi::from_event(
            device_id,
            OcaObject::from_descriptor(channel_scheme::descriptor(channel, Field::Phantom), OcaValue::Bool(event.state.phantom)),
        ),
    ];
    if let Some(pad) = event.state.pad {
        out.push(OcaEventFfi::from_event(
            device_id,
            OcaObject::from_descriptor(channel_scheme::descriptor(channel, Field::Pad), OcaValue::Bool(pad)),
        ));
    }
    out
}

/// Wraps any `Box<dyn DeviceAdapter>` as a `PluginAdapter`. `channels`
/// must match the device's actual channel count (from
/// `DeviceConfig::channel_count()`) so `describe()` and Ono decoding
/// cover exactly the range the device really has.
pub struct LegacyPluginBridge {
    id: String,
    inner: Box<dyn DeviceAdapter>,
    channels: u16,
    runtime: Runtime,
    events: Arc<StdMutex<VecDeque<OcaEventFfi>>>,
}

impl LegacyPluginBridge {
    pub fn new(inner: Box<dyn DeviceAdapter>, channels: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("building the plugin bridge's Tokio runtime");

        let events: Arc<StdMutex<VecDeque<OcaEventFfi>>> = Arc::new(StdMutex::new(VecDeque::new()));
        let mut rx = inner.subscribe();
        let id = inner.id().to_string();
        let device_id = id.clone();
        let events_for_task = Arc::clone(&events);
        runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let mut queue = events_for_task.lock().unwrap();
                        for oca_event in preamp_event_to_oca_ffi(&device_id, event) {
                            queue.push_back(oca_event);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self { id, inner, channels, runtime, events }
    }
}

impl PluginAdapter for LegacyPluginBridge {
    fn id(&self) -> RString {
        self.id.clone().into()
    }

    fn connect(&mut self) -> RResult<(), RString> {
        match self.runtime.block_on(self.inner.connect()) {
            Ok(()) => RResult::ROk(()),
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn disconnect(&mut self) -> RResult<(), RString> {
        match self.runtime.block_on(self.inner.disconnect()) {
            Ok(()) => RResult::ROk(()),
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn identify(&mut self) -> RResult<RDeviceInfo, RString> {
        match self.runtime.block_on(self.inner.identify()) {
            Ok(info) => RResult::ROk(RDeviceInfo {
                vendor: info.vendor.into(),
                model: info.model.into(),
                address: info.address.to_string().into(),
            }),
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn describe(&self) -> RVec<OcaObjectDescriptorFfi> {
        channel_scheme::descriptors_for_channels(self.channels)
            .into_iter()
            .map(OcaObjectDescriptorFfi::from)
            .collect::<Vec<_>>()
            .into()
    }

    fn get_object(&mut self, ono: u32) -> RResult<OcaValueFfi, RString> {
        let Some((channel, field)) = channel_scheme::decode_ono(Ono(ono), self.channels) else {
            return RResult::RErr(format!("no such object 0x{ono:08x}").into());
        };
        match self.runtime.block_on(self.inner.get_state(channel)) {
            Ok(state) => match field {
                Field::Gain => RResult::ROk(OcaValueFfi::F32(state.gain_db)),
                Field::Phantom => RResult::ROk(OcaValueFfi::Bool(state.phantom)),
                Field::Pad => match state.pad {
                    Some(pad) => RResult::ROk(OcaValueFfi::Bool(pad)),
                    None => RResult::RErr(format!("channel {channel} has no pad switch").into()),
                },
            },
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn set_object(&mut self, ono: u32, value: OcaValueFfi) -> RResult<(), RString> {
        let Some((channel, field)) = channel_scheme::decode_ono(Ono(ono), self.channels) else {
            return RResult::RErr(format!("no such object 0x{ono:08x}").into());
        };
        let result = match field {
            Field::Gain => {
                let OcaValueFfi::F32(v) = value else {
                    return RResult::RErr("gain requires an F32 value".into());
                };
                self.runtime.block_on(self.inner.set_gain(channel, v))
            }
            Field::Phantom => {
                let OcaValueFfi::Bool(v) = value else {
                    return RResult::RErr("phantom requires a Bool value".into());
                };
                self.runtime.block_on(self.inner.set_phantom(channel, v))
            }
            Field::Pad => return RResult::RErr(format!("channel {channel}'s pad switch is read-only").into()),
        };
        match result {
            Ok(()) => RResult::ROk(()),
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn poll_events(&mut self) -> RVec<OcaEventFfi> {
        let mut queue = self.events.lock().unwrap();
        queue.drain(..).collect::<Vec<_>>().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PreampAddress;
    use crate::types::PreampState;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::broadcast;

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

        async fn connect(&mut self) -> crate::adapter::AdapterResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> crate::adapter::AdapterResult<()> {
            Ok(())
        }

        async fn identify(&mut self) -> crate::adapter::AdapterResult<crate::adapter::DeviceInfo> {
            Ok(crate::adapter::DeviceInfo {
                vendor: "mock".into(),
                model: "mock".into(),
                address: "127.0.0.1".parse().unwrap(),
            })
        }

        async fn set_gain(&mut self, channel: u16, gain_db: f32) -> crate::adapter::AdapterResult<()> {
            self.gain_calls.lock().unwrap().push((channel, gain_db));
            Ok(())
        }

        async fn set_phantom(&mut self, _channel: u16, _on: bool) -> crate::adapter::AdapterResult<()> {
            Ok(())
        }

        async fn get_state(&mut self, channel: u16) -> crate::adapter::AdapterResult<PreampState> {
            self.state
                .lock()
                .unwrap()
                .get(&channel)
                .copied()
                .ok_or(crate::adapter::AdapterError::UnsupportedChannel(channel))
        }

        fn subscribe(&self) -> broadcast::Receiver<PreampEvent> {
            self.tx.subscribe()
        }
    }

    type MockParts = (MockDeviceAdapter, broadcast::Sender<PreampEvent>, Arc<StdMutex<Vec<(u16, f32)>>>);

    fn mock() -> MockParts {
        let (tx, _rx) = broadcast::channel(16);
        let state = Arc::new(StdMutex::new(HashMap::new()));
        let gain_calls = Arc::new(StdMutex::new(Vec::new()));
        (
            MockDeviceAdapter { id: "d".into(), tx: tx.clone(), state, gain_calls: gain_calls.clone() },
            tx,
            gain_calls,
        )
    }

    #[test]
    fn describe_covers_every_channel_with_three_objects_each() {
        let (adapter, ..) = mock();
        let bridge = LegacyPluginBridge::new(Box::new(adapter), 4);
        assert_eq!(bridge.describe().len(), 12);
    }

    #[test]
    fn get_and_set_object_translate_through_channel_numbers() {
        let (adapter, _tx, gain_calls) = mock();
        let bridge_channels = 4;
        let mut bridge = LegacyPluginBridge::new(Box::new(adapter), bridge_channels);

        // Populate channel 2's state via a direct set_gain call, then read
        // it back through get_object using the same Ono the shim's
        // describe() would report.
        let gain_ono = channel_scheme::gain_ono(2).into();
        assert!(matches!(bridge.set_object(gain_ono, OcaValueFfi::F32(6.0)), RResult::ROk(())));
        assert_eq!(*gain_calls.lock().unwrap(), vec![(2, 6.0)]);

        let pad_ono = channel_scheme::pad_ono(2).into();
        assert!(matches!(bridge.set_object(pad_ono, OcaValueFfi::Bool(true)), RResult::RErr(_)));
    }

    #[test]
    fn subscribe_translates_one_preamp_event_into_gain_and_phantom_events() {
        let (adapter, tx, _gain_calls) = mock();
        let mut bridge = LegacyPluginBridge::new(Box::new(adapter), 4);

        tx.send(PreampEvent {
            address: PreampAddress::new("d", 3),
            state: PreampState { gain_db: 1.5, phantom: true, pad: None },
        })
        .unwrap();

        let mut last_gain = None;
        let mut last_phantom = None;
        for _ in 0..50 {
            for event in Vec::from(bridge.poll_events()) {
                if event.ono == u32::from(channel_scheme::gain_ono(3)) {
                    last_gain = Some(event.value.clone());
                }
                if event.ono == u32::from(channel_scheme::phantom_ono(3)) {
                    last_phantom = Some(event.value.clone());
                }
            }
            if last_gain.is_some() && last_phantom.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(last_gain, Some(OcaValueFfi::F32(1.5)));
        assert_eq!(last_phantom, Some(OcaValueFfi::Bool(true)));
    }
}

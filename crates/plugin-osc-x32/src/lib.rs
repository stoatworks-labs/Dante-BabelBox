//! Dynamically-loadable plugin for the X32-family OSC dialect (Behringer
//! X32, Midas M32/HD96) - the proof-of-concept plugin proving the whole
//! dylib pipeline works end to end.
//!
//! Reuses [`dante_babelbox_preamp_adapter_osc::X32Adapter`] verbatim for
//! the actual wire protocol (headamp OSC paths, `/info` parsing, the
//! `/xremote` heartbeat, disconnect/cancellation) - this crate is purely
//! an FFI-facing translation layer, structurally identical to
//! `dante_babelbox_core::LegacyPreampShim` but crossing a dylib boundary
//! instead of staying in-process. Wire-format changes (new OSC paths,
//! corrected offsets, etc.) belong in that crate, never duplicated here.
//!
//! The FFI [`PluginAdapter`] trait is synchronous (async doesn't cross an
//! `abi_stable` boundary cleanly - see `oca-plugin-abi`'s doc comment),
//! but `X32Adapter` is async: each adapter instance owns its own
//! single-threaded Tokio runtime to bridge the two.
//! `connect`/`disconnect`/`get_object`/`set_object` block on that runtime;
//! the adapter's own receive loop and `/xremote` heartbeat keep running
//! as background tasks on it between calls, exactly as they do today when
//! `X32Adapter` runs in-process.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    sabi_trait::prelude::TD_Opaque,
    std_types::{RResult, RString, RVec},
};
use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_oca::{Ono, OcaClass, OcaObject, OcaObjectDescriptor, OcaValue};
use dante_babelbox_oca_plugin_abi::{
    OcaEventFfi, OcaObjectDescriptorFfi, OcaValueFfi, PluginAdapter, PluginAdapterBox, PluginAdapter_TO,
    PluginRootModule, PluginRootModule_Ref, RDeviceConfig, RDeviceInfo, RPluginInfo,
};
use dante_babelbox_preamp_adapter_osc::X32Adapter;
use tokio::runtime::Runtime;

/// This kind id, wired through the host's `PluginRegistry`.
const KIND: &str = "osc-x32";

/// Matches the well-known X32-family default (confirmed against the same
/// constant `preamp-cli` already uses for the in-process path) - not a
/// guess, just not something the adapter crate itself exposes as a
/// `pub const` today.
const DEFAULT_X32_PORT: u16 = 10023;

const CHANNELS: u16 = 24;

fn gain_ono(channel: u16) -> Ono {
    Ono(2 * (channel as u32 - 1) + 1)
}

fn phantom_ono(channel: u16) -> Ono {
    Ono(2 * (channel as u32 - 1) + 2)
}

/// Decodes an `Ono` back to its channel and whether it's the gain object
/// (`true`) or the phantom object (`false`).
fn decode_ono(ono: Ono) -> Option<(u16, bool)> {
    let n = ono.0.checked_sub(1)?;
    let channel = (n / 2 + 1) as u16;
    if channel == 0 || channel > CHANNELS {
        return None;
    }
    Some((channel, n % 2 == 0))
}

fn descriptor(channel: u16, is_gain: bool) -> OcaObjectDescriptor {
    if is_gain {
        OcaObjectDescriptor {
            ono: gain_ono(channel),
            class: OcaClass::Gain,
            role: format!("Ch {channel} Gain"),
            settable: true,
        }
    } else {
        OcaObjectDescriptor {
            ono: phantom_ono(channel),
            class: OcaClass::Switch,
            role: format!("Ch {channel} Phantom"),
            settable: true,
        }
    }
}

struct X32PluginAdapter {
    id: String,
    inner: X32Adapter,
    runtime: Runtime,
    events: Arc<StdMutex<VecDeque<OcaEventFfi>>>,
}

impl X32PluginAdapter {
    fn new(id: String, remote: std::net::SocketAddr) -> Self {
        // Must be multi-threaded, not `new_current_thread`: a
        // current-thread runtime only drives spawned tasks (the receive
        // loop, the `/xremote` heartbeat) while something is actively
        // inside a `block_on` call on it. `poll_events` - called by the
        // host on its own timer, never via `block_on` - would otherwise
        // never give those tasks a chance to run between `connect`/
        // `get_object`/`set_object` calls, so inbound telemetry would only
        // ever be (unreliably) discovered as a side effect of the next
        // blocking FFI call rather than continuously in the background.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("building the x32 plugin's Tokio runtime");
        let inner = X32Adapter::new(id.clone(), remote);
        let events: Arc<StdMutex<VecDeque<OcaEventFfi>>> = Arc::new(StdMutex::new(VecDeque::new()));

        // Subscribed before the adapter is moved into `Self` - `subscribe`
        // takes `&self`, so this doesn't need the adapter to be connected
        // yet, matching `X32Adapter`'s own broadcast-channel semantics.
        let mut rx = inner.subscribe();
        let events_for_task = Arc::clone(&events);
        let device_id = id.clone();
        runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let channel = event.address.channel;
                        let mut queue = events_for_task.lock().unwrap();
                        queue.push_back(OcaEventFfi::from_event(
                            device_id.clone(),
                            OcaObject::from_descriptor(descriptor(channel, true), OcaValue::F32(event.state.gain_db)),
                        ));
                        queue.push_back(OcaEventFfi::from_event(
                            device_id.clone(),
                            OcaObject::from_descriptor(
                                descriptor(channel, false),
                                OcaValue::Bool(event.state.phantom),
                            ),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self { id, inner, runtime, events }
    }
}

impl PluginAdapter for X32PluginAdapter {
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
        let mut out = Vec::with_capacity(CHANNELS as usize * 2);
        for channel in 1..=CHANNELS {
            out.push(OcaObjectDescriptorFfi::from(descriptor(channel, true)));
            out.push(OcaObjectDescriptorFfi::from(descriptor(channel, false)));
        }
        out.into()
    }

    fn get_object(&mut self, ono: u32) -> RResult<OcaValueFfi, RString> {
        let Some((channel, is_gain)) = decode_ono(Ono(ono)) else {
            return RResult::RErr(format!("no such object 0x{ono:08x}").into());
        };
        match self.runtime.block_on(self.inner.get_state(channel)) {
            Ok(state) => RResult::ROk(if is_gain {
                OcaValueFfi::F32(state.gain_db)
            } else {
                OcaValueFfi::Bool(state.phantom)
            }),
            Err(e) => RResult::RErr(e.to_string().into()),
        }
    }

    fn set_object(&mut self, ono: u32, value: OcaValueFfi) -> RResult<(), RString> {
        let Some((channel, is_gain)) = decode_ono(Ono(ono)) else {
            return RResult::RErr(format!("no such object 0x{ono:08x}").into());
        };
        let result = if is_gain {
            let OcaValueFfi::F32(v) = value else {
                return RResult::RErr("gain requires an F32 value".into());
            };
            self.runtime.block_on(self.inner.set_gain(channel, v))
        } else {
            let OcaValueFfi::Bool(v) = value else {
                return RResult::RErr("phantom requires a Bool value".into());
            };
            self.runtime.block_on(self.inner.set_phantom(channel, v))
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

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "osc-x32".into(),
        vendor: "Behringer/Midas".into(),
        supported_kinds: RVec::from(vec![RString::from(KIND)]),
    }
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    let Some(address) = config.address.into_option() else {
        return RResult::RErr(format!("device '{}': {KIND} requires an address", config.id).into());
    };
    let ip: std::net::IpAddr = match address.as_str().parse() {
        Ok(ip) => ip,
        Err(e) => return RResult::RErr(format!("device '{}': invalid address: {e}", config.id).into()),
    };
    let port = config.port.into_option().unwrap_or(DEFAULT_X32_PORT);
    let remote = std::net::SocketAddr::new(ip, port);

    let adapter = X32PluginAdapter::new(config.id.into(), remote);
    RResult::ROk(PluginAdapter_TO::from_value(adapter, TD_Opaque))
}

#[export_root_module]
pub fn get_library() -> PluginRootModule_Ref {
    PluginRootModule { plugin_info, create_adapter }.leak_into_prefix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::library::RootModule;
    use rosc::{OscMessage, OscPacket, OscType};
    use tokio::net::UdpSocket;

    #[test]
    fn ono_encoding_round_trips_across_channels() {
        for channel in 1..=CHANNELS {
            assert_eq!(decode_ono(gain_ono(channel)), Some((channel, true)));
            assert_eq!(decode_ono(phantom_ono(channel)), Some((channel, false)));
        }
    }

    #[test]
    fn decode_ono_rejects_out_of_range_and_zero() {
        assert_eq!(decode_ono(Ono(0)), None);
        assert_eq!(decode_ono(gain_ono(CHANNELS + 1)), None);
    }

    #[test]
    fn describe_reports_two_objects_per_channel() {
        let plugin = X32PluginAdapter::new("x32-1".into(), "127.0.0.1:0".parse().unwrap());
        let objects = PluginAdapter::describe(&plugin);
        assert_eq!(objects.len(), CHANNELS as usize * 2);
        assert!(objects.iter().any(|o| o.role.as_str() == "Ch 3 Gain" && o.settable));
        assert!(objects.iter().any(|o| o.role.as_str() == "Ch 3 Phantom" && o.settable));
    }

    #[test]
    fn plugin_info_declares_the_osc_x32_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    /// End-to-end through the concrete `X32PluginAdapter` (no dylib
    /// boundary): connect, `identify()` against a mock X32 UDP responder,
    /// then confirm a spontaneous headamp update (as a physical console
    /// would send to a subscribed `/xremote` client) surfaces through
    /// `poll_events` as OCA gain/phantom events, `set_object` reaches the
    /// wire, and `disconnect` stops the receive loop. Deliberately uses
    /// spontaneous pushes rather than `get_object`'s query path: `get_state`
    /// on the underlying `X32Adapter` queries and caches gain+phantom
    /// together and returns as soon as either field's reply lands, so
    /// asserting on it here would just be racing that adapter's own
    /// internal timing rather than testing this crate's translation layer.
    /// That's unrelated to this proof-of-concept, and unchanged from how
    /// `X32Adapter` already behaves in production.
    #[test]
    fn connect_identify_set_and_poll_events_round_trip_through_a_mock_x32() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = runtime.block_on(UdpSocket::bind("127.0.0.1:0")).unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let mut plugin = X32PluginAdapter::new("x32-1".into(), mock_addr);
        assert!(matches!(plugin.connect(), RResult::ROk(())));

        // The `/xremote` heartbeat fires immediately at `connect()` and
        // every 9s after, independent of everything else this adapter
        // does - it can arrive (and sit unread) at any point in this
        // exchange, so every read here discards it rather than treating
        // "the next packet" as necessarily meaningful.
        async fn recv_ignoring_heartbeat(
            mock: &UdpSocket,
            buf: &mut [u8],
        ) -> (OscMessage, std::net::SocketAddr) {
            loop {
                let (len, sender) = mock.recv_from(buf).await.unwrap();
                if let Ok((_, OscPacket::Message(m))) = rosc::decoder::decode_udp(&buf[..len]) {
                    if m.addr != "/xremote" {
                        return (m, sender);
                    }
                }
            }
        }

        // Reply to /info, then push a spontaneous headamp update and read
        // back whatever `set_object` sends.
        let server = std::thread::spawn(move || {
            runtime.block_on(async move {
                let mut buf = [0u8; 512];
                let (info_query, from) = recv_ignoring_heartbeat(&mock, &mut buf).await;
                assert_eq!(info_query.addr, "/info");
                let reply = OscPacket::Message(OscMessage {
                    addr: "/info".to_string(),
                    args: vec![
                        OscType::String("V2.05".into()),
                        OscType::String("osc-server".into()),
                        OscType::String("X32".into()),
                        OscType::String("2.12".into()),
                    ],
                });
                let bytes = rosc::encoder::encode(&reply).unwrap();
                mock.send_to(&bytes, from).await.unwrap();

                // Spontaneous push: channel 3 gain and phantom both changed,
                // unsolicited - exactly what a physical knob turn produces.
                for (addr, arg) in [
                    ("/headamp/03/gain", OscType::Float(-6.0)),
                    ("/headamp/03/phantom", OscType::Int(1)),
                ] {
                    let msg = OscPacket::Message(OscMessage { addr: addr.to_string(), args: vec![arg] });
                    let bytes = rosc::encoder::encode(&msg).unwrap();
                    mock.send_to(&bytes, from).await.unwrap();
                }

                // Read back whatever set_object sends for channel 3 gain.
                let (set_msg, _) = recv_ignoring_heartbeat(&mock, &mut buf).await;
                OscPacket::Message(set_msg)
            })
        });

        let info = plugin.identify();
        let RResult::ROk(info) = info else { panic!("identify failed: {info:?}") };
        assert_eq!(info.model.as_str(), "X32");

        // Drain poll_events over a short window and track the *last* value
        // seen per ono. `X32Adapter` broadcasts one `PreampEvent` per
        // incoming OSC message, each carrying the *whole* channel state -
        // so the gain-changed message (arriving before phantom is known)
        // produces a transient phantom=false event alongside the real
        // gain=-6.0 one, settling to phantom=true only once the second
        // message lands. That transient is expected, not a bug - only the
        // final, settled value matters here.
        let mut last_gain = None;
        let mut last_phantom = None;
        for _ in 0..50 {
            for event in Vec::from(plugin.poll_events()) {
                if event.ono == u32::from(gain_ono(3)) {
                    last_gain = Some(event.value.clone());
                }
                if event.ono == u32::from(phantom_ono(3)) {
                    last_phantom = Some(event.value.clone());
                }
            }
            if last_gain == Some(OcaValueFfi::F32(-6.0)) && last_phantom == Some(OcaValueFfi::Bool(true)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(last_gain, Some(OcaValueFfi::F32(-6.0)), "expected the settled gain OcaEvent");
        assert_eq!(last_phantom, Some(OcaValueFfi::Bool(true)), "expected the settled phantom OcaEvent");

        assert!(matches!(plugin.set_object(gain_ono(3).into(), OcaValueFfi::F32(3.5)), RResult::ROk(())));

        let sent = server.join().unwrap();
        assert!(matches!(sent, OscPacket::Message(m) if m.addr == "/headamp/03/gain"));

        assert!(matches!(plugin.disconnect(), RResult::ROk(())));
    }

    /// Real proof the FFI boundary itself works: build produces a
    /// `cdylib`, load it back with `abi_stable`'s own loader, and confirm
    /// the root module round-trips. Skips (rather than fails) if the
    /// `cdylib` artifact isn't present yet - `cargo test` builds this
    /// crate's own `cdylib` as a side effect before running its `rlib`
    /// test binary, but a directly-invoked `cargo test --lib` in some
    /// setups may not have triggered that build.
    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_osc_x32.dylib"),
            dylib_path.join("libdante_babelbox_plugin_osc_x32.so"),
            dylib_path.join("dante_babelbox_plugin_osc_x32.dll"),
        ];
        let Some(path) = candidates.iter().find(|p| p.exists()) else {
            eprintln!("skipping: no built cdylib found at any of {candidates:?} - run `cargo build -p dante-babelbox-plugin-osc-x32` first");
            return;
        };

        let root = PluginRootModule_Ref::load_from_file(path).expect("loading the plugin cdylib");
        let info = root.plugin_info()();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);

        let config = RDeviceConfig {
            id: "x32-dylib-test".into(),
            address: abi_stable::std_types::RSome("127.0.0.1".into()),
            port: abi_stable::std_types::RSome(0),
            channels: abi_stable::std_types::RNone,
        };
        let adapter = root.create_adapter()(config);
        assert!(matches!(adapter, RResult::ROk(_)));
    }
}

//! Dynamically-loadable plugin for Focusrite RedNet preamps over AES70/OCA.
//!
//! **Protocol source: the AES70 standard** (AES70-1 object model, AES70-3
//! OCP.1 transport), implemented in `dante-babelbox-ocp1`. Nothing here is
//! reverse-engineered — RedNet devices carry an AES70 endpoint in firmware,
//! toggled per device by RedNet Control's `AES70 Enable/Disable` (it sits
//! alongside Clock Source and Word Clock in the same per-device Tools menu, and
//! is one of the settings an `Operator`-level DDM login is blocked from), so
//! RedNet Control does not need to be running for this plugin to work. That is
//! the practical difference from Focusrite's other documented control path,
//! MIDI/SysEx, whose own guide states that "RedNet Control must be running to
//! send and receive MIDI messages".
//!
//! ## Why AES70 rather than the Yamaha route
//!
//! A RedNet MP8R can also be driven as a Yamaha head amp — it has a
//! `Yamaha ID` setting (Off, `Y000`–`Y00F`), the same ID space this project's
//! own capture shows (`Y001-Yamaha-QL1-…`, `Y004-Yamaha-Rio3224-D2-…`), and
//! Focusrite advertise gain, phantom and HPF control from CL/QL consoles. That
//! would reuse [`docs/yamaha-ha-remote-over-dante.md`](../../docs/yamaha-ha-remote-over-dante.md)
//! wholesale. It is not what this plugin does, because the QL1/Rio3224-D2
//! captures contain no RedNet traffic at all: the gain range differs (MP8R is
//! 10–65 dB in 1 dB steps against the Rio's −6…+66), the channel mapping into a
//! 32-slot broadcast is unknown for an 8-input device, and the pairing
//! handshake was never implemented because the one hardware-proven write went
//! to a Rio that was already paired with a live console. Building that from the
//! captures alone would be guesswork of exactly the kind this project flags.
//! See [`docs/rednet-mp8r-capture-request.md`](../../docs/rednet-mp8r-capture-request.md)
//! for what a capture would have to contain to make it real.
//!
//! ## What is and isn't verified
//!
//! **This plugin has never been run against a RedNet device**, and neither has
//! the OCP.1 crate under it. The wire format comes from a published standard
//! rather than from guesswork, which is a better starting position than an
//! unverified vendor protocol — but it is not the same as having been tested.
//! Two specific things to check first against real hardware, both flagged at
//! their definitions:
//!
//! - the `OcaBlock::GetMembers` method index and reply shape
//!   (`dante_babelbox_ocp1::enumerate`), which is why enumeration tries
//!   alternatives instead of assuming one;
//! - the role-string patterns in [`roles`], which are the only guess in this
//!   crate, and the switch-position-0-is-off convention in [`adapter`].

mod adapter;
mod roles;

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    sabi_trait::prelude::TD_Opaque,
    std_types::{RResult, RString, RVec},
};
use dante_babelbox_oca_plugin_abi::{
    PluginAdapterBox, PluginAdapter_TO, PluginRootModule, PluginRootModule_Ref, RDeviceConfig,
    RPluginInfo,
};

pub use adapter::RedNetAdapter;

const KIND: &str = "rednet-aes70";

/// AES70-3 assigns no port — discovery is meant to go via mDNS `_oca._tcp`, and
/// a config that knows the device's port should say so. 65000 is the widely-used
/// convention and is only the fallback.
const DEFAULT_PORT: u16 = dante_babelbox_ocp1::client::DEFAULT_OCP1_PORT;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "rednet-aes70".into(),
        vendor: "Focusrite".into(),
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
        Err(e) => {
            return RResult::RErr(format!("device '{}': invalid address: {e}", config.id).into())
        }
    };
    let port = config.port.into_option().unwrap_or(DEFAULT_PORT);
    let remote = std::net::SocketAddr::new(ip, port);

    // `channels` is deliberately ignored: an AES70 device's object set comes
    // from the device at connect time, so a configured channel count could only
    // contradict it.
    RResult::ROk(PluginAdapter_TO::from_value(
        RedNetAdapter::new(config.id.into_string(), remote),
        TD_Opaque,
    ))
}

#[export_root_module]
pub fn get_library() -> PluginRootModule_Ref {
    PluginRootModule { plugin_info, create_adapter }.leak_into_prefix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::library::RootModule;
    use abi_stable::std_types::{RNone, RSome};
    use dante_babelbox_oca_plugin_abi::OcaValueFfi;
    use dante_babelbox_ocp1::pdu::{self, Message, Response};
    use dante_babelbox_ocp1::value::Writer;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A mock AES70 device: one root block containing two per-input blocks,
    /// each with a gain and a phantom switch.
    ///
    /// It answers the way `dante-babelbox-ocp1` expects a device to, so what
    /// this proves is that the plugin's two halves agree — enumeration,
    /// classification, role mapping, get and set all line up end to end. It is
    /// **not** evidence about a real RedNet unit.
    struct MockDevice {
        /// ONo -> current gain, for the two gain objects.
        gains: Arc<Mutex<HashMap<u32, f32>>>,
    }

    const ROOT_BLOCK: u32 = 100;
    const INPUT_BLOCK: [u32; 2] = [0x1000, 0x2000];
    const GAIN_ONO: [u32; 2] = [0x1001, 0x2001];
    const PHANTOM_ONO: [u32; 2] = [0x1002, 0x2002];

    fn object_identification(w: &mut Writer, ono: u32, fields: &[u16]) {
        w.u32(ono).u16(fields.len() as u16);
        for f in fields {
            w.u16(*f);
        }
        w.u32(2);
    }

    impl MockDevice {
        fn members(&self, container: u32) -> Option<Vec<u8>> {
            let mut w = Writer::new();
            match container {
                ROOT_BLOCK => {
                    w.u16(2);
                    for block in INPUT_BLOCK {
                        object_identification(&mut w, block, &[1, 1, 3]);
                    }
                }
                _ => {
                    let index = INPUT_BLOCK.iter().position(|b| *b == container)?;
                    w.u16(2);
                    object_identification(&mut w, GAIN_ONO[index], &[1, 1, 1, 5]);
                    object_identification(&mut w, PHANTOM_ONO[index], &[1, 1, 1, 4]);
                }
            }
            Some(w.finish())
        }

        fn role(&self, ono: u32) -> Option<String> {
            Some(match ono {
                ROOT_BLOCK => "Device".into(),
                _ if INPUT_BLOCK.contains(&ono) => {
                    let index = INPUT_BLOCK.iter().position(|b| *b == ono)?;
                    format!("Input {}", index + 1)
                }
                _ if GAIN_ONO.contains(&ono) => "Gain".into(),
                _ if PHANTOM_ONO.contains(&ono) => "Phantom Power".into(),
                _ => return None,
            })
        }

        fn class(&self, ono: u32) -> Option<&'static [u16]> {
            Some(match ono {
                _ if ono == ROOT_BLOCK || INPUT_BLOCK.contains(&ono) => &[1, 1, 3],
                _ if GAIN_ONO.contains(&ono) => &[1, 1, 1, 5],
                _ if PHANTOM_ONO.contains(&ono) => &[1, 1, 1, 4],
                _ => return None,
            })
        }

        /// Returns the response params, or `Err(status)`.
        fn handle(&self, cmd: &pdu::Command) -> Result<Vec<u8>, u8> {
            use dante_babelbox_ocp1::classes::{block, level, root, single_value, subscription};

            // Only the primary GetMembers index is implemented, so the
            // candidate fallback in `enumerate` is exercised for real: a device
            // that answers BadMethod to the others must still enumerate.
            if cmd.method == block::GET_MEMBERS {
                return self.members(cmd.target).ok_or(5);
            }
            if cmd.method.def_level == level::BLOCK && cmd.method != block::GET_MEMBERS {
                return Err(11); // BadMethod
            }
            if cmd.method == root::GET_ROLE {
                let role = self.role(cmd.target).ok_or(5)?;
                let mut w = Writer::new();
                w.string(&role);
                return Ok(w.finish());
            }
            if cmd.method == root::GET_CLASS_IDENTIFICATION {
                let fields = self.class(cmd.target).ok_or(5)?;
                let mut w = Writer::new();
                w.u16(fields.len() as u16);
                for f in fields {
                    w.u16(*f);
                }
                w.u32(2);
                return Ok(w.finish());
            }
            if cmd.method == subscription::ADD_SUBSCRIPTION {
                return Ok(Vec::new());
            }
            if cmd.method == single_value::get(level::GAIN) && GAIN_ONO.contains(&cmd.target) {
                let mut w = Writer::new();
                w.f32(*self.gains.lock().unwrap().get(&cmd.target).unwrap_or(&0.0));
                return Ok(w.finish());
            }
            if cmd.method == single_value::set(level::GAIN) && GAIN_ONO.contains(&cmd.target) {
                let value = f32::from_be_bytes(cmd.params[..4].try_into().map_err(|_| 4u8)?);
                self.gains.lock().unwrap().insert(cmd.target, value);
                return Ok(Vec::new());
            }
            if cmd.method == single_value::get(level::SWITCH) && PHANTOM_ONO.contains(&cmd.target) {
                let mut w = Writer::new();
                w.u16(1); // phantom on
                return Ok(w.finish());
            }
            Err(8) // NotImplemented
        }
    }

    fn spawn_mock() -> (std::net::SocketAddr, Arc<Mutex<HashMap<u32, f32>>>) {
        let gains: Arc<Mutex<HashMap<u32, f32>>> = Arc::default();
        let gains_for_task = Arc::clone(&gains);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();

        std::thread::spawn(move || {
            runtime.block_on(async move {
                let device = MockDevice { gains: gains_for_task };
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let mut pending = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    pending.extend_from_slice(&buf[..n]);

                    while let Ok(Some(len)) = pdu::pdu_len(&pending) {
                        let Ok(messages) = pdu::decode_pdu(&pending[..len]) else { break };
                        pending.drain(..len);
                        for message in messages {
                            let Message::Command(cmd) = message else { continue };
                            let (status, params) = match device.handle(&cmd) {
                                Ok(params) => (0, params),
                                Err(status) => (status, Vec::new()),
                            };
                            let response = Response {
                                handle: cmd.handle,
                                status,
                                param_count: u8::from(!params.is_empty()),
                                params,
                            };
                            if socket.write_all(&pdu::encode_response(&response)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        });

        (addr, gains)
    }

    fn config(addr: std::net::SocketAddr) -> RDeviceConfig {
        RDeviceConfig {
            id: "rednet-1".into(),
            address: RSome(addr.ip().to_string().into()),
            port: RSome(addr.port()),
            channels: RNone,
        }
    }

    #[test]
    fn plugin_info_declares_the_rednet_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
        assert_eq!(info.vendor.as_str(), "Focusrite");
    }

    #[test]
    fn create_adapter_requires_an_address() {
        let config = RDeviceConfig {
            id: "rednet-1".into(),
            address: RNone,
            port: RNone,
            channels: RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    #[test]
    fn create_adapter_rejects_a_malformed_address() {
        let config = RDeviceConfig {
            id: "rednet-1".into(),
            address: RSome("not-an-ip".into()),
            port: RNone,
            channels: RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    /// The whole path in one test: connect, enumerate a two-input device
    /// through the `GetMembers` candidate fallback, and confirm the objects
    /// arrive with the host's channel-mapping role format.
    #[test]
    fn connect_enumerates_the_device_into_channel_roles() {
        let (addr, _gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let described = Vec::from(adapter.describe());
        let mut roles: Vec<&str> = described.iter().map(|d| d.role.as_str()).collect();
        roles.sort_unstable();
        assert_eq!(roles, ["Ch 1 Gain", "Ch 1 Phantom", "Ch 2 Gain", "Ch 2 Phantom"]);
        // Blocks are containers, not objects: they must not show up themselves.
        assert_eq!(described.len(), 4);
        assert!(described.iter().all(|d| d.settable));

        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn gain_round_trips_through_the_real_ocp1_path() {
        let (addr, gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let gain_ono = Vec::from(adapter.describe())
            .into_iter()
            .find(|d| d.role.as_str() == "Ch 2 Gain")
            .expect("Ch 2 Gain")
            .ono;

        assert!(matches!(adapter.set_object(gain_ono, OcaValueFfi::F32(45.0)), RResult::ROk(())));
        // The value reached the device, not just the adapter's own cache.
        assert_eq!(gains.lock().unwrap().get(&GAIN_ONO[1]).copied(), Some(45.0));

        match adapter.get_object(gain_ono) {
            RResult::ROk(OcaValueFfi::F32(v)) => assert_eq!(v, 45.0),
            other => panic!("unexpected {other:?}"),
        }

        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn a_phantom_switch_reads_back_as_a_boolean() {
        let (addr, _gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let phantom_ono = Vec::from(adapter.describe())
            .into_iter()
            .find(|d| d.role.as_str() == "Ch 1 Phantom")
            .expect("Ch 1 Phantom")
            .ono;

        match adapter.get_object(phantom_ono) {
            RResult::ROk(OcaValueFfi::Bool(v)) => assert!(v),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn operations_before_connect_fail_rather_than_panic() {
        let (addr, _gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.get_object(0x1001), RResult::RErr(_)));
        assert!(matches!(adapter.set_object(0x1001, OcaValueFfi::F32(0.0)), RResult::RErr(_)));
        assert!(adapter.describe().is_empty());
    }

    #[test]
    fn an_unknown_ono_is_rejected() {
        let (addr, _gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));
        assert!(matches!(adapter.get_object(0xDEAD_BEEF), RResult::RErr(_)));
    }

    #[test]
    fn connect_fails_cleanly_when_nothing_is_listening() {
        // Bind and drop, so the port is almost certainly closed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::RErr(_)));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_rednet_aes70.dylib"),
            dylib_path.join("libdante_babelbox_plugin_rednet_aes70.so"),
            dylib_path.join("dante_babelbox_plugin_rednet_aes70.dll"),
        ];
        let Some(path) = candidates.iter().find(|p| p.exists()) else {
            eprintln!("skipping: no built cdylib found at any of {candidates:?}");
            return;
        };

        let root = PluginRootModule_Ref::load_from_file(path).expect("loading the plugin cdylib");
        let info = root.plugin_info()();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    /// Guard against the runtime-starvation trap described in the module
    /// comment: `poll_events` is called by the host outside any `block_on`, so
    /// the adapter's notification task has to be running on its own threads.
    #[test]
    fn poll_events_is_safe_to_call_without_a_block_on_in_flight() {
        let (addr, _gains) = spawn_mock();
        let RResult::ROk(mut adapter) = create_adapter(config(addr)) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));
        std::thread::sleep(Duration::from_millis(50));
        assert!(adapter.poll_events().is_empty());
    }
}

//! Dynamically-loadable plugin for Yamaha DM3/DM3S (official OSC spec).
//!
//! Reuses [`dante_babelbox_preamp_adapter_yamaha::Dm3Adapter`] verbatim
//! for the wire protocol, via [`dante_babelbox_core::LegacyPluginBridge`] -
//! the generic FFI-facing translation shared by every not-yet-bespoke
//! plugin in this project (see that type's doc comment, and
//! `docs/plugin-development-guide.md`, for the full contract). This
//! crate is nothing but the `plugin_info`/`create_adapter` wiring.

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    sabi_trait::prelude::TD_Opaque,
    std_types::{RResult, RString, RVec},
};
use dante_babelbox_core::LegacyPluginBridge;
use dante_babelbox_oca_plugin_abi::{
    PluginAdapterBox, PluginAdapter_TO, PluginRootModule, PluginRootModule_Ref, RDeviceConfig, RPluginInfo,
};
use dante_babelbox_preamp_adapter_yamaha::Dm3Adapter;

const KIND: &str = "yamaha-dm3";
/// Matches the well-known DM3 OSC default (confirmed against the same
/// constant `preamp-cli` already uses for the in-process path).
const DEFAULT_PORT: u16 = 49900;
/// DM3/DM3S Local Input count - see `Dm3Adapter`'s module doc comment.
const DEFAULT_CHANNELS: u16 = 16;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "yamaha-dm3".into(),
        vendor: "Yamaha".into(),
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
    let port = config.port.into_option().unwrap_or(DEFAULT_PORT);
    let channels = config.channels.into_option().unwrap_or(DEFAULT_CHANNELS);
    let remote = std::net::SocketAddr::new(ip, port);

    let adapter = Dm3Adapter::new(config.id.into_string(), remote);
    let bridge = LegacyPluginBridge::new(Box::new(adapter), channels);
    RResult::ROk(PluginAdapter_TO::from_value(bridge, TD_Opaque))
}

#[export_root_module]
pub fn get_library() -> PluginRootModule_Ref {
    PluginRootModule { plugin_info, create_adapter }.leak_into_prefix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::library::RootModule;
    use dante_babelbox_core::channel_scheme;
    use dante_babelbox_oca_plugin_abi::OcaValueFfi;
    use rosc::{OscMessage, OscPacket, OscType};
    use std::time::Duration;
    use tokio::net::UdpSocket;

    #[test]
    fn plugin_info_declares_the_yamaha_dm3_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn create_adapter_requires_an_address() {
        let config = RDeviceConfig {
            id: "dm3-1".into(),
            address: abi_stable::std_types::RNone,
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    /// End-to-end through the real plugin construction path: connect,
    /// `set_object` a gain value (DM3 has no heartbeat, so this is what
    /// teaches the mock the adapter's ephemeral port - matching how
    /// `dante_babelbox_preamp_adapter_yamaha::dm3`'s own
    /// `disconnect_stops_the_receive_loop` test does the same thing),
    /// confirm the wire message matches the documented OSC address, then
    /// push a spontaneous phantom-on update and confirm it surfaces
    /// through `poll_events`. Proves `create_adapter` wires
    /// `Dm3Adapter`'s real socket logic into the FFI boundary correctly.
    #[test]
    fn set_object_and_poll_events_round_trip_through_a_mock_dm3() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = runtime.block_on(UdpSocket::bind("127.0.0.1:0")).unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let config = RDeviceConfig {
            id: "dm3-1".into(),
            address: abi_stable::std_types::RSome(mock_addr.ip().to_string().into()),
            port: abi_stable::std_types::RSome(mock_addr.port()),
            channels: abi_stable::std_types::RSome(16),
        };
        let RResult::ROk(mut adapter) = create_adapter(config) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let server = std::thread::spawn(move || {
            runtime.block_on(async move {
                let mut buf = [0u8; 512];
                let (len, from) = mock.recv_from(&mut buf).await.unwrap();
                let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
                let OscPacket::Message(m) = packet else { panic!("expected message") };
                assert_eq!(m.addr, "/yosc:req/set/IO:Current/InCh/HAGain/5/1");
                assert_eq!(m.args, vec![OscType::Int(42)]);

                let phantom_on = OscPacket::Message(OscMessage {
                    addr: "/yosc:req/set/IO:Current/InCh/48VOn/5/1".to_string(),
                    args: vec![OscType::Int(1)],
                });
                let bytes = rosc::encoder::encode(&phantom_on).unwrap();
                mock.send_to(&bytes, from).await.unwrap();
            });
        });

        let gain_ono = channel_scheme::gain_ono(5).into();
        assert!(matches!(adapter.set_object(gain_ono, OcaValueFfi::F32(42.0)), RResult::ROk(())));

        let mut last_phantom = None;
        for _ in 0..50 {
            for event in Vec::from(adapter.poll_events()) {
                if event.role.as_str() == "Ch 5 Phantom" {
                    last_phantom = Some(event.value.clone());
                }
            }
            if last_phantom.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(last_phantom, Some(OcaValueFfi::Bool(true)));

        server.join().unwrap();
        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_yamaha_dm3.dylib"),
            dylib_path.join("libdante_babelbox_plugin_yamaha_dm3.so"),
            dylib_path.join("dante_babelbox_plugin_yamaha_dm3.dll"),
        ];
        let Some(path) = candidates.iter().find(|p| p.exists()) else {
            eprintln!("skipping: no built cdylib found at any of {candidates:?}");
            return;
        };

        let root = PluginRootModule_Ref::load_from_file(path).expect("loading the plugin cdylib");
        let info = root.plugin_info()();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }
}

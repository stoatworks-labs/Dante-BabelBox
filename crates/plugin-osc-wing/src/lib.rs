//! Dynamically-loadable plugin for Behringer Wing's own OSC dialect
//! (console's 8 built-in LCL preamps).
//!
//! Reuses [`dante_babelbox_preamp_adapter_osc::WingAdapter`] verbatim for
//! the wire protocol, via [`dante_babelbox_core::LegacyPluginBridge`] -
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
use dante_babelbox_preamp_adapter_osc::WingAdapter;

const KIND: &str = "osc-wing";
/// Matches the well-known Wing OSC default (confirmed against the same
/// constant `preamp-cli` already uses for the in-process path).
const DEFAULT_PORT: u16 = 2223;
/// Wing's own 8 built-in LCL preamps - see `WingAdapter`'s module doc
/// comment for why this crate only covers those today.
const DEFAULT_CHANNELS: u16 = 8;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "osc-wing".into(),
        vendor: "Behringer".into(),
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

    let adapter = WingAdapter::new(config.id.into_string(), remote);
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
    use rosc::{OscMessage, OscPacket, OscType};
    use std::time::Duration;
    use tokio::net::UdpSocket;

    #[test]
    fn plugin_info_declares_the_osc_wing_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn create_adapter_requires_an_address() {
        let config = RDeviceConfig {
            id: "wing-1".into(),
            address: abi_stable::std_types::RNone,
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    /// End-to-end through the real plugin construction path: connect,
    /// then confirm a spontaneous headamp update (as WING would push to a
    /// subscribed client) surfaces through `poll_events` as OCA gain/
    /// phantom events. Proves `create_adapter` wires `WingAdapter`'s real
    /// socket logic into the FFI boundary correctly - the translation
    /// logic itself is already covered by `LegacyPluginBridge`'s own
    /// tests in `dante-babelbox-core`.
    #[test]
    fn connect_and_poll_events_round_trip_through_a_mock_wing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mock = runtime.block_on(UdpSocket::bind("127.0.0.1:0")).unwrap();
        let mock_addr = mock.local_addr().unwrap();

        let config = RDeviceConfig {
            id: "wing-1".into(),
            address: abi_stable::std_types::RSome(mock_addr.ip().to_string().into()),
            port: abi_stable::std_types::RSome(mock_addr.port()),
            channels: abi_stable::std_types::RSome(8),
        };
        let RResult::ROk(mut adapter) = create_adapter(config) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        // WING's own /*s subscription-renewal heartbeat, then a spontaneous
        // gain+phantom update for channel 3.
        let server = std::thread::spawn(move || {
            runtime.block_on(async move {
                let mut buf = [0u8; 512];
                let (_, from) = mock.recv_from(&mut buf).await.unwrap(); // /*s heartbeat

                for (addr, arg) in [
                    ("/io/in/LCL/3/g", OscType::Float(6.0)),
                    ("/io/in/LCL/3/vph", OscType::Int(1)),
                ] {
                    let msg = OscPacket::Message(OscMessage { addr: addr.to_string(), args: vec![arg] });
                    let bytes = rosc::encoder::encode(&msg).unwrap();
                    mock.send_to(&bytes, from).await.unwrap();
                }
            });
        });

        let mut last_gain = None;
        let mut last_phantom = None;
        for _ in 0..50 {
            for event in Vec::from(adapter.poll_events()) {
                if event.role.as_str() == "Ch 3 Gain" {
                    last_gain = Some(event.value.clone());
                }
                if event.role.as_str() == "Ch 3 Phantom" {
                    last_phantom = Some(event.value.clone());
                }
            }
            if last_gain.is_some() && last_phantom.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(last_gain, Some(dante_babelbox_oca_plugin_abi::OcaValueFfi::F32(6.0)));
        assert_eq!(last_phantom, Some(dante_babelbox_oca_plugin_abi::OcaValueFfi::Bool(true)));

        server.join().unwrap();
        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_osc_wing.dylib"),
            dylib_path.join("libdante_babelbox_plugin_osc_wing.so"),
            dylib_path.join("dante_babelbox_plugin_osc_wing.dll"),
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

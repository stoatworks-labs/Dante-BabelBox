//! Dynamically-loadable plugin for Allen & Heath dLive (MIDI-over-TCP,
//! physical preamp "Socket" addressing).
//!
//! Reuses [`dante_babelbox_preamp_adapter_ah::DliveAdapter`] verbatim for
//! the wire protocol, via [`dante_babelbox_core::LegacyPluginBridge`] -
//! the generic FFI-facing translation shared by every not-yet-bespoke
//! plugin in this project (see that type's doc comment, and
//! `docs/plugin-development-guide.md`, for the full contract). This
//! crate is nothing but the `plugin_info`/`create_adapter` wiring.
//!
//! `channel` here means dLive's physical preamp Socket number (1-128,
//! covering MixRack + DX1/2 + DX3/4), not a processing channel - see
//! `DliveAdapter`'s own module doc comment.

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
use dante_babelbox_preamp_adapter_ah::DliveAdapter;

const KIND: &str = "dlive-tcp";
/// Matches the well-known dLive MixRack default (confirmed against the
/// same constant `preamp-cli` already uses for the in-process path).
const DEFAULT_PORT: u16 = 51325;
const DEFAULT_CHANNELS: u16 = 128;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "dlive-tcp".into(),
        vendor: "Allen & Heath".into(),
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

    let adapter = DliveAdapter::new(config.id.into_string(), remote);
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
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    const SYSEX_HEADER: [u8; 8] = [0xF0, 0x00, 0x00, 0x1A, 0x50, 0x10, 0x01, 0x00];
    const OP_48V_STATUS: u8 = 0x0B;

    #[test]
    fn plugin_info_declares_the_dlive_tcp_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn create_adapter_requires_an_address() {
        let config = RDeviceConfig {
            id: "dlive-1".into(),
            address: abi_stable::std_types::RNone,
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    /// End-to-end through the real plugin construction path: connect,
    /// then confirm a spontaneous 48V-on push for socket 65 (as a dLive
    /// unit would send after a physical/on-screen change) surfaces
    /// through `poll_events` as an OCA event. The exact SysEx sequence
    /// below is the same one `dante_babelbox_preamp_adapter_ah::dlive`'s
    /// own tests use. Proves `create_adapter` wires `DliveAdapter`'s real
    /// socket logic into the FFI boundary correctly - the translation
    /// logic itself is already covered by `LegacyPluginBridge`'s own
    /// tests in `dante-babelbox-core`.
    #[test]
    fn connect_and_poll_events_round_trip_through_a_mock_dlive() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            runtime.block_on(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut msg = SYSEX_HEADER.to_vec();
                msg.extend_from_slice(&[0x00, OP_48V_STATUS, 0x40, 0x7F, 0xF7]); // socket 65
                socket.write_all(&msg).await.unwrap();
            });
        });

        let config = RDeviceConfig {
            id: "dlive-1".into(),
            address: abi_stable::std_types::RSome(addr.ip().to_string().into()),
            port: abi_stable::std_types::RSome(addr.port()),
            channels: abi_stable::std_types::RSome(128),
        };
        let RResult::ROk(mut adapter) = create_adapter(config) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let mut last_phantom = None;
        for _ in 0..50 {
            for event in Vec::from(adapter.poll_events()) {
                if event.role.as_str() == "Ch 65 Phantom" {
                    last_phantom = Some(event.value.clone());
                }
            }
            if last_phantom.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(last_phantom, Some(dante_babelbox_oca_plugin_abi::OcaValueFfi::Bool(true)));

        server.join().unwrap();
        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_dlive_tcp.dylib"),
            dylib_path.join("libdante_babelbox_plugin_dlive_tcp.so"),
            dylib_path.join("dante_babelbox_plugin_dlive_tcp.dll"),
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

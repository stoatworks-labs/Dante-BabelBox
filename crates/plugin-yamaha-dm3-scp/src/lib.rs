//! Dynamically-loadable plugin for Yamaha DM3/DM3S over **SCP** (TCP 49280).
//!
//! Kind `yamaha-dm3-scp`. Wraps
//! [`dante_babelbox_preamp_adapter_yamaha::Dm3ScpAdapter`] via
//! [`dante_babelbox_core::LegacyPluginBridge`] - the same generic FFI
//! translation the other thin plugins use.
//!
//! This is the **recommended** DM3 target and the only one in the project
//! validated end-to-end against real hardware (read, write, identify, and a
//! live change feed). The sibling `plugin-yamaha-dm3` (kind `yamaha-dm3`,
//! OSC on UDP 49900) remains for hosts that need the OSC transport, but SCP
//! is strictly better: confirmed reads/writes and unsolicited `NOTIFY`
//! push, neither of which OSC offers. See
//! [`Dm3ScpAdapter`](dante_babelbox_preamp_adapter_yamaha::Dm3ScpAdapter)'s
//! module doc and `docs/yamaha-dm3-bench-2026-09-15.md`.

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
use dante_babelbox_preamp_adapter_yamaha::{Dm3ScpAdapter, SCP_PORT};

const KIND: &str = "yamaha-dm3-scp";
/// DM3/DM3S Local Input count.
const DEFAULT_CHANNELS: u16 = 16;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "yamaha-dm3-scp".into(),
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
    let port = config.port.into_option().unwrap_or(SCP_PORT);
    let channels = config.channels.into_option().unwrap_or(DEFAULT_CHANNELS);
    let remote = std::net::SocketAddr::new(ip, port);

    let adapter = Dm3ScpAdapter::new(config.id.into_string(), remote);
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
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn plugin_info_declares_the_scp_kind() {
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

    /// End-to-end through the real plugin construction path: connect over a
    /// mock TCP SCP server, `set_object` a gain, confirm the documented SCP
    /// line reaches the wire, then push an unsolicited `NOTIFY` and confirm
    /// it surfaces through `poll_events`. Mirrors the OSC plugin's own
    /// round-trip test, over SCP.
    #[test]
    fn set_object_and_poll_events_round_trip_through_a_mock_dm3() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            runtime.block_on(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 512];
                let mut acc = String::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return acc;
                    }
                    acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if acc.contains("set IO:Current/InCh/HAGain") {
                        // acknowledge and push a phantom NOTIFY for ch 5 (idx 4)
                        sock.write_all(b"NOTIFY set IO:Current/InCh/48VOn 4 0 1 \"ON\"\n")
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        return acc;
                    }
                }
            })
        });

        let config = RDeviceConfig {
            id: "dm3-1".into(),
            address: abi_stable::std_types::RSome(addr.ip().to_string().into()),
            port: abi_stable::std_types::RSome(addr.port()),
            channels: abi_stable::std_types::RSome(16),
        };
        let RResult::ROk(mut adapter) = create_adapter(config) else {
            panic!("create_adapter failed")
        };
        assert!(matches!(adapter.connect(), RResult::ROk(())));

        let gain_ono = channel_scheme::gain_ono(5).into();
        assert!(matches!(adapter.set_object(gain_ono, OcaValueFfi::F32(30.0)), RResult::ROk(())));

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

        let seen = server.join().unwrap();
        assert!(seen.contains("set IO:Current/InCh/HAGain 4 0 30"), "got: {seen:?}");
        assert!(matches!(adapter.disconnect(), RResult::ROk(())));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dylib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dylib_path.join("libdante_babelbox_plugin_yamaha_dm3_scp.dylib"),
            dylib_path.join("libdante_babelbox_plugin_yamaha_dm3_scp.so"),
            dylib_path.join("dante_babelbox_plugin_yamaha_dm3_scp.dll"),
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

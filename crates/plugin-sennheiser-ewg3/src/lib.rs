//! Dynamically-loadable plugin for the Sennheiser evolution-wireless G3
//! receiver.
//!
//! Reuses [`dante_babelbox_preamp_adapter_sennheiser_ewg3::Ewg3Adapter`]
//! verbatim for the wire protocol, via
//! [`dante_babelbox_core::LegacyPluginBridge`]. This crate is nothing but
//! the `plugin_info`/`create_adapter` wiring, matching every other plugin
//! here (see `docs/plugin-development-guide.md`).
//!
//! `kind = "sennheiser-ewg3"`. One channel (the receiver's AF-out level).
//! The `port` config field is ignored: the WSM binary protocol is fixed to
//! UDP 8133. The receiver's IP goes in `address`.

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    sabi_trait::prelude::TD_Opaque,
    std_types::{RResult, RString, RVec},
};
use dante_babelbox_core::LegacyPluginBridge;
use dante_babelbox_oca_plugin_abi::{
    PluginAdapterBox, PluginAdapter_TO, PluginRootModule, PluginRootModule_Ref, RDeviceConfig,
    RPluginInfo,
};
use dante_babelbox_preamp_adapter_sennheiser_ewg3::Ewg3Adapter;

const KIND: &str = "sennheiser-ewg3";
/// The G3 exposes a single audio output.
const DEFAULT_CHANNELS: u16 = 1;

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "sennheiser-ewg3".into(),
        vendor: "Sennheiser".into(),
        supported_kinds: RVec::from(vec![RString::from(KIND)]),
    }
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    let Some(address) = config.address.into_option() else {
        return RResult::RErr(format!("device '{}': {KIND} requires an address", config.id).into());
    };
    let ip: std::net::Ipv4Addr = match address.as_str().parse() {
        Ok(ip) => ip,
        Err(e) => {
            return RResult::RErr(
                format!("device '{}': invalid IPv4 address '{address}': {e}", config.id).into(),
            )
        }
    };
    let channels = config.channels.into_option().unwrap_or(DEFAULT_CHANNELS);

    let adapter = Ewg3Adapter::new(config.id.into_string(), ip);
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

    #[test]
    fn plugin_info_declares_the_sennheiser_ewg3_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn create_adapter_requires_an_address() {
        let config = RDeviceConfig {
            id: "g3-1".into(),
            address: abi_stable::std_types::RNone,
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    #[test]
    fn create_adapter_rejects_a_non_ipv4_address() {
        let config = RDeviceConfig {
            id: "g3-1".into(),
            address: abi_stable::std_types::RSome("not-an-ip".into()),
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::RErr(_)));
    }

    #[test]
    fn create_adapter_accepts_a_valid_receiver_address() {
        let config = RDeviceConfig {
            id: "g3-1".into(),
            address: abi_stable::std_types::RSome("192.168.0.101".into()),
            port: abi_stable::std_types::RNone,
            channels: abi_stable::std_types::RNone,
        };
        assert!(matches!(create_adapter(config), RResult::ROk(_)));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dir.join("libdante_babelbox_plugin_sennheiser_ewg3.dylib"),
            dir.join("libdante_babelbox_plugin_sennheiser_ewg3.so"),
            dir.join("dante_babelbox_plugin_sennheiser_ewg3.dll"),
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

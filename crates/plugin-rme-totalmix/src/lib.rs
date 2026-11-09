//! Dynamically-loadable plugin for RME TotalMix FX output faders.
//!
//! Reuses [`dante_babelbox_preamp_adapter_rme_totalmix::TotalMixAdapter`]
//! for the wire protocol, via [`dante_babelbox_core::LegacyPluginBridge`].
//! This crate is nothing but the `plugin_info`/`create_adapter` wiring,
//! matching every other plugin here (see `docs/plugin-development-guide.md`).
//!
//! `kind = "rme-totalmix"`. `address` is the computer running TotalMix
//! (usually 127.0.0.1), `port` its incoming OSC port (7001 if left out),
//! and `channels` how many hardware outputs to expose, which has to be
//! given: it depends on the interface. Replies are read on UDP 9001,
//! TotalMix's default outgoing port.

use std::net::{IpAddr, SocketAddr};

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
use dante_babelbox_preamp_adapter_rme_totalmix::{TotalMixAdapter, TOTALMIX_PORT};

const KIND: &str = "rme-totalmix";

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "rme-totalmix".into(),
        vendor: "RME".into(),
        supported_kinds: RVec::from(vec![RString::from(KIND)]),
    }
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    let Some(address) = config.address.into_option() else {
        return RResult::RErr(
            format!(
                "device '{}': {KIND} requires the address of the computer running TotalMix",
                config.id
            )
            .into(),
        );
    };
    let ip: IpAddr = match address.as_str().parse() {
        Ok(ip) => ip,
        Err(e) => {
            return RResult::RErr(
                format!(
                    "device '{}': invalid IP address '{address}': {e}",
                    config.id
                )
                .into(),
            )
        }
    };
    let Some(channels) = config.channels.into_option().filter(|c| *c > 0) else {
        return RResult::RErr(
            format!(
                "device '{}': {KIND} needs `channels`, the number of TotalMix hardware outputs to expose",
                config.id
            )
            .into(),
        );
    };
    let port = config.port.into_option().unwrap_or(TOTALMIX_PORT);

    let adapter =
        TotalMixAdapter::new(config.id.into_string(), SocketAddr::new(ip, port), channels);
    let bridge = LegacyPluginBridge::new(Box::new(adapter), channels);
    RResult::ROk(PluginAdapter_TO::from_value(bridge, TD_Opaque))
}

#[export_root_module]
pub fn get_library() -> PluginRootModule_Ref {
    PluginRootModule {
        plugin_info,
        create_adapter,
    }
    .leak_into_prefix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::library::RootModule;
    use abi_stable::std_types::{RNone, RSome};

    fn config(address: Option<&str>, channels: Option<u16>) -> RDeviceConfig {
        RDeviceConfig {
            id: "digiface".into(),
            address: address.map(|a| RSome(a.into())).unwrap_or(RNone),
            port: RNone,
            channels: channels.map(RSome).unwrap_or(RNone),
        }
    }

    #[test]
    fn plugin_info_declares_the_rme_totalmix_kind() {
        let info = plugin_info();
        assert_eq!(info.supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn create_adapter_requires_an_address() {
        assert!(matches!(
            create_adapter(config(None, Some(8))),
            RResult::RErr(_)
        ));
    }

    #[test]
    fn create_adapter_rejects_an_address_that_is_not_an_ip() {
        assert!(matches!(
            create_adapter(config(Some("totalmix.local"), Some(8))),
            RResult::RErr(_)
        ));
    }

    #[test]
    fn create_adapter_requires_a_channel_count() {
        assert!(matches!(
            create_adapter(config(Some("127.0.0.1"), None)),
            RResult::RErr(_)
        ));
        assert!(matches!(
            create_adapter(config(Some("127.0.0.1"), Some(0))),
            RResult::RErr(_)
        ));
    }

    #[test]
    fn create_adapter_accepts_a_totalmix_host_and_count() {
        assert!(matches!(
            create_adapter(config(Some("127.0.0.1"), Some(8))),
            RResult::ROk(_)
        ));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dir.join("libdante_babelbox_plugin_rme_totalmix.dylib"),
            dir.join("libdante_babelbox_plugin_rme_totalmix.so"),
            dir.join("dante_babelbox_plugin_rme_totalmix.dll"),
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

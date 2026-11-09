//! Dynamically-loadable plugin for an **emulated** Yamaha Rio: a stagebox
//! that exists only on the Dante network, for a Yamaha console to control.
//!
//! Kind `yamaha-rio-emulated`. Wraps
//! [`dante_babelbox_preamp_adapter_yamaha::EmulatedRioAdapter`] via
//! [`dante_babelbox_core::LegacyPluginBridge`], like every other thin
//! plugin here. What it does and what has and has not been proven is in the
//! adapter's module doc and in `docs/dante-emulation.md`.
//!
//! ```toml
//! [[device]]
//! id = "virtual-rio"
//! kind = "yamaha-rio-emulated"
//! address = "169.254.10.20"   # THIS machine's address on the Dante network
//! unit_id = 5                 # the console mounts it as Y005
//! ```
//!
//! Unlike every other kind, `address` is not somewhere to dial. It is the
//! local interface the device is presented on. `port` is ignored.
//!
//! | Key | Default | |
//! |---|---|---|
//! | `unit_id` | required | Stagebox unit id, 1 to 4095 (`0x1F` style is accepted). Must not clash with a real unit. |
//! | `model` | `Rio3224-D2` | The only model there is captured evidence for. |
//! | `name` | built | Dante device name. Built as `Y<unit>-Yamaha-<model>-<mac suffix>`. |
//! | `mac` | derived | The MAC the device claims, e.g. `02:bb:5a:01:02:03`. Derived from the id and address if left out. |
//! | `advertise` | `true` | Announce the device over mDNS. |
//! | `presence` | `true` | Answer Dante's own queries (ARC, ConMon). Off, only the head-amp protocol runs. |
//! | `meters` | `true` | Stream (silent) input meters while a console asks for them. |
//! | `arc_port`, `cmc_port`, `control_port` | 4440, 8800, 8700 | `0` lets the system choose. A standard port that is taken is replaced the same way. |

use std::net::Ipv4Addr;

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    sabi_trait::prelude::TD_Opaque,
    std_types::{RResult, RString, RVec},
};
use dante_babelbox_core::LegacyPluginBridge;
use dante_babelbox_emulation::{parse_mac, validate_name};
use dante_babelbox_oca_plugin_abi::{
    PluginAdapterBox, PluginAdapter_TO, PluginRootModule, PluginRootModule_Ref, RDeviceConfig, RPluginInfo,
};
use dante_babelbox_preamp_adapter_yamaha::{EmulatedRioAdapter, EmulatedRioConfig, RSeriesModel};

const KIND: &str = "yamaha-rio-emulated";

#[sabi_extern_fn]
fn plugin_info() -> RPluginInfo {
    RPluginInfo {
        name: "yamaha-rio-emulated".into(),
        vendor: "Yamaha".into(),
        supported_kinds: RVec::from(vec![RString::from(KIND)]),
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("`{key}` must be true or false, not '{other}'")),
    }
}

/// Decimal, or hex with a `0x` prefix: a unit id is set in hex on the
/// stagebox's front panel and shown in hex in its Dante name.
fn parse_number(key: &str, value: &str) -> Result<u32, String> {
    let parsed = match value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
        Some(digits) => u32::from_str_radix(digits, 16),
        None => value.parse(),
    };
    parsed.map_err(|_| format!("`{key}` must be a whole number, not '{value}'"))
}

fn parse_port(key: &str, value: &str) -> Result<u16, String> {
    u16::try_from(parse_number(key, value)?).map_err(|_| format!("`{key}` must be a port number, not '{value}'"))
}

/// Turns the `[[device]]` table into the adapter's configuration, or says
/// what is wrong with it in words that name the key.
fn parse_config(config: &RDeviceConfig) -> Result<EmulatedRioConfig, String> {
    let address = config.address.as_ref().into_option().ok_or_else(|| {
        format!("{KIND} needs `address`: this machine's own address on the Dante network, where the device will appear")
    })?;
    let interface: Ipv4Addr = address
        .as_str()
        .parse()
        .map_err(|_| format!("`address` must be an IPv4 address of this machine, not '{address}'"))?;

    let unit_id = config
        .option("unit_id")
        .ok_or_else(|| format!("{KIND} needs `unit_id`: the stagebox unit id the console will mount (5 is Y005)"))?;
    let unit_id = parse_number("unit_id", unit_id)?;
    if !(1..=0xfff).contains(&unit_id) {
        return Err(format!("`unit_id` must be between 1 and 4095 (0xFFF), not {unit_id}"));
    }

    let mut out = EmulatedRioConfig::new(interface, unit_id as u16);
    if let Some(model) = config.option("model") {
        let known = RSeriesModel::rio3224_d2();
        if !model.eq_ignore_ascii_case(&known.name) {
            return Err(format!(
                "`model` '{model}' is not supported: {} is the only model whose answers have been captured",
                known.name
            ));
        }
    }
    if let Some(mac) = config.option("mac") {
        out.mac = Some(parse_mac(mac).map_err(|e| e.to_string())?);
    }
    if let Some(name) = config.option("name") {
        validate_name(name).map_err(|e| e.to_string())?;
        out.name = Some(name.to_string());
    }
    for (key, target) in [
        ("advertise", &mut out.advertise),
        ("presence", &mut out.presence),
        ("meters", &mut out.meters),
    ] {
        if let Some(value) = config.option(key) {
            *target = parse_bool(key, value)?;
        }
    }
    for (key, target) in [
        ("arc_port", &mut out.ports.arc),
        ("cmc_port", &mut out.ports.cmc),
        ("control_port", &mut out.ports.control),
    ] {
        if let Some(value) = config.option(key) {
            *target = parse_port(key, value)?;
        }
    }

    // The name is checked here, built or given, so a bad one is a
    // configuration error and not a failure to start.
    let identity = out.identity(config.id.as_str());
    validate_name(&identity.name).map_err(|e| e.to_string())?;
    Ok(out)
}

#[sabi_extern_fn]
fn create_adapter(config: RDeviceConfig) -> RResult<PluginAdapterBox, RString> {
    let parsed = match parse_config(&config) {
        Ok(parsed) => parsed,
        Err(e) => return RResult::RErr(format!("device '{}': {e}", config.id).into()),
    };
    let adapter = EmulatedRioAdapter::new(config.id.as_str(), parsed);
    let available = adapter.channels();
    let channels = match config.channels.into_option() {
        Some(asked) if asked == 0 || asked > available => {
            return RResult::RErr(
                format!("device '{}': `channels` must be between 1 and {available}, not {asked}", config.id).into(),
            )
        }
        Some(asked) => asked,
        None => available,
    };
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
    use abi_stable::std_types::{RNone, RSome, Tuple2};

    fn config(address: Option<&str>, options: &[(&str, &str)]) -> RDeviceConfig {
        RDeviceConfig {
            id: "virtual-rio".into(),
            address: address.map(|a| RSome(a.into())).unwrap_or(RNone),
            port: RNone,
            channels: RNone,
            options: options.iter().map(|(k, v)| Tuple2(RString::from(*k), RString::from(*v))).collect(),
        }
    }

    fn error(config: RDeviceConfig) -> String {
        parse_config(&config).expect_err("a configuration error")
    }

    #[test]
    fn plugin_info_declares_the_emulated_rio_kind() {
        assert_eq!(plugin_info().supported_kinds.as_slice(), &[RString::from(KIND)]);
    }

    #[test]
    fn an_address_and_a_unit_id_are_enough() {
        let parsed = parse_config(&config(Some("169.254.10.20"), &[("unit_id", "5")])).unwrap();
        assert_eq!(parsed.unit_id, 5);
        assert_eq!(parsed.interface, Ipv4Addr::new(169, 254, 10, 20));
        assert!(parsed.advertise && parsed.presence && parsed.meters);
        let name = parsed.identity("virtual-rio").name;
        assert!(name.starts_with("Y005-Yamaha-Rio3224-D2-"), "{name}");
        assert!(matches!(create_adapter(config(Some("169.254.10.20"), &[("unit_id", "5")])), RResult::ROk(_)));
    }

    #[test]
    fn a_unit_id_can_be_written_the_way_the_front_panel_shows_it() {
        let parsed = parse_config(&config(Some("10.0.0.5"), &[("unit_id", "0x1F")])).unwrap();
        assert_eq!(parsed.unit_id, 31);
        assert!(parsed.identity("virtual-rio").name.starts_with("Y01F-"));
    }

    #[test]
    fn every_option_is_read() {
        let parsed = parse_config(&config(
            Some("10.0.0.5"),
            &[
                ("unit_id", "7"),
                ("model", "rio3224-d2"),
                ("mac", "02:bb:5a:01:02:03"),
                ("name", "Y007-Yamaha-Rio3224-D2-010203"),
                ("advertise", "false"),
                ("presence", "false"),
                ("meters", "false"),
                ("arc_port", "0"),
                ("cmc_port", "38800"),
                ("control_port", "38700"),
            ],
        ))
        .unwrap();
        assert_eq!(parsed.mac, Some([0x02, 0xbb, 0x5a, 0x01, 0x02, 0x03]));
        assert_eq!(parsed.name.as_deref(), Some("Y007-Yamaha-Rio3224-D2-010203"));
        assert!(!parsed.advertise && !parsed.presence && !parsed.meters);
        assert_eq!((parsed.ports.arc, parsed.ports.cmc, parsed.ports.control), (0, 38800, 38700));
    }

    #[test]
    fn each_mistake_is_named_by_its_key() {
        assert!(error(config(None, &[("unit_id", "5")])).contains("`address`"));
        assert!(error(config(Some("rio.local"), &[("unit_id", "5")])).contains("`address`"));
        assert!(error(config(Some("10.0.0.5"), &[])).contains("`unit_id`"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "0")])).contains("between 1 and 4095"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5000")])).contains("between 1 and 4095"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "five")])).contains("`unit_id`"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5"), ("model", "Tio1608-D")])).contains("Rio3224-D2"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5"), ("mac", "nope")])).contains("MAC"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5"), ("name", "has space")])).contains("Dante name"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5"), ("advertise", "yes")])).contains("`advertise`"));
        assert!(error(config(Some("10.0.0.5"), &[("unit_id", "5"), ("arc_port", "70000")])).contains("`arc_port`"));
    }

    #[test]
    fn a_channel_count_beyond_the_model_is_refused() {
        let mut too_many = config(Some("10.0.0.5"), &[("unit_id", "5")]);
        too_many.channels = RSome(48);
        assert!(matches!(create_adapter(too_many), RResult::RErr(_)));
        let mut fewer = config(Some("10.0.0.5"), &[("unit_id", "5")]);
        fewer.channels = RSome(8);
        assert!(matches!(create_adapter(fewer), RResult::ROk(_)));
    }

    #[test]
    fn the_built_cdylib_loads_through_abi_stables_own_loader() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
        let candidates = [
            dir.join("libdante_babelbox_plugin_yamaha_rio_emulated.dylib"),
            dir.join("libdante_babelbox_plugin_yamaha_rio_emulated.so"),
            dir.join("dante_babelbox_plugin_yamaha_rio_emulated.dll"),
        ];
        let Some(path) = candidates.iter().find(|p| p.exists()) else {
            eprintln!("skipping: no built cdylib found at any of {candidates:?}");
            return;
        };
        let root = PluginRootModule_Ref::load_from_file(path).expect("loading the plugin cdylib");
        assert_eq!(root.plugin_info()().supported_kinds.as_slice(), &[RString::from(KIND)]);
    }
}

//! Auto-generates the `[[device]]` blocks of a `bridge.toml` by discovering
//! Dante devices via mDNS and probing each found address against every
//! implemented adapter's `identify()` until one claims it.
//!
//! `[[mapping]]` entries are left for the user to add by hand by default,
//! since deciding channel intent generally isn't something the network
//! can answer. With `--infer-mappings`, `init` additionally observes
//! Dante's own audio-routing/subscription protocol (separate from every
//! vendor's preamp-control protocol - see [`dante_babelbox_discovery::dante_control`])
//! to guess mappings from live patching. That's a real signal, not a
//! guess, but it comes with a caveat serious enough to keep it opt-in:
//! see [`infer_mappings`]'s doc comment.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use dante_babelbox_preamp_adapter_ah::{AhmAdapter, DliveAdapter};
use dante_babelbox_preamp_adapter_osc::{WingAdapter, X32Adapter};
use dante_babelbox_preamp_adapter_yamaha::Dm3Adapter;
use dante_babelbox_core::{ChannelMapping, DeviceAdapter, DeviceConfig, PreampAddress};
use dante_babelbox_discovery::dante_control;
use serde::Serialize;
use tracing::{debug, info};

use crate::config::Config;
use crate::ports::{DEFAULT_AHM_PORT, DEFAULT_DLIVE_PORT, DEFAULT_DM3_PORT, DEFAULT_WING_PORT, DEFAULT_X32_PORT};

/// The protocol kinds `init` knows how to probe for - a subset of the
/// registered kind ids (excludes `ah-midi` and `yamaha`, which have no
/// adapter). Tried in this order for every discovered IP; AHM and dLive share port
/// 51325 by default, so both get tried against it.
#[derive(Debug, Clone, Copy)]
enum ProbeKind {
    OscWing,
    OscX32,
    AhTcp,
    DliveTcp,
    YamahaDm3,
}

impl ProbeKind {
    const ALL: [ProbeKind; 5] = [
        ProbeKind::OscWing,
        ProbeKind::OscX32,
        ProbeKind::AhTcp,
        ProbeKind::DliveTcp,
        ProbeKind::YamahaDm3,
    ];

    fn default_port(self) -> u16 {
        match self {
            ProbeKind::OscWing => DEFAULT_WING_PORT,
            ProbeKind::OscX32 => DEFAULT_X32_PORT,
            ProbeKind::AhTcp => DEFAULT_AHM_PORT,
            ProbeKind::DliveTcp => DEFAULT_DLIVE_PORT,
            ProbeKind::YamahaDm3 => DEFAULT_DM3_PORT,
        }
    }

    /// The device "kind" id to record in `DeviceConfig.kind` - just
    /// `slug()` as an owned `String`, kept as a separate method (rather
    /// than callers using `slug().to_string()` directly) so the "this is
    /// the kind id" intent is named at the call site.
    fn as_device_kind(self) -> String {
        self.slug().to_string()
    }

    fn slug(self) -> &'static str {
        match self {
            ProbeKind::OscWing => "osc-wing",
            ProbeKind::OscX32 => "osc-x32",
            ProbeKind::AhTcp => "ah-tcp",
            ProbeKind::DliveTcp => "dlive-tcp",
            ProbeKind::YamahaDm3 => "yamaha-dm3",
        }
    }

    fn build_adapter(self, addr: SocketAddr) -> Box<dyn DeviceAdapter> {
        match self {
            ProbeKind::OscWing => Box::new(WingAdapter::new("probe", addr)),
            ProbeKind::OscX32 => Box::new(X32Adapter::new("probe", addr)),
            ProbeKind::AhTcp => Box::new(AhmAdapter::new("probe", addr)),
            ProbeKind::DliveTcp => Box::new(DliveAdapter::new("probe", addr)),
            ProbeKind::YamahaDm3 => Box::new(Dm3Adapter::new("probe", addr)),
        }
    }
}

/// Tries each `ProbeKind` in turn against `ip`, each attempt (connect +
/// identify combined) bounded by `per_attempt_timeout`. Returns the first
/// kind that successfully identifies, if any.
async fn probe_ip(ip: IpAddr, per_attempt_timeout: Duration) -> Option<(ProbeKind, u16)> {
    for kind in ProbeKind::ALL {
        let port = kind.default_port();
        let addr = SocketAddr::new(ip, port);
        let attempt = tokio::time::timeout(per_attempt_timeout, async move {
            let mut adapter = kind.build_adapter(addr);
            adapter.connect().await?;
            adapter.identify().await
        });
        if let Ok(Ok(info)) = attempt.await {
            info!(%ip, port, vendor = %info.vendor, model = %info.model, "identified device");
            return Some((kind, port));
        }
    }
    None
}

fn last_octet(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.octets()[3].to_string(),
        IpAddr::V6(v6) => v6.segments()[7].to_string(),
    }
}

fn dedupe_id(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Testable core: no mDNS or CLI concerns, just IPs in, a `Config` with
/// `[[device]]` entries (and empty `mappings`) out.
pub async fn probe_and_build(ips: Vec<IpAddr>, per_attempt_timeout: Duration) -> Config {
    let mut devices = Vec::new();
    let mut used_ids: HashSet<String> = HashSet::new();

    for ip in ips {
        let Some((kind, port)) = probe_ip(ip, per_attempt_timeout).await else {
            continue;
        };
        let id = dedupe_id(format!("{}-{}", kind.slug(), last_octet(ip)), &mut used_ids);
        devices.push(DeviceConfig {
            id,
            kind: kind.as_device_kind(),
            address: Some(ip),
            port: Some(port),
            is_virtual: false,
            channels: None,
        });
    }

    Config {
        devices,
        mappings: Vec::new(),
    }
}

/// Observes live Dante audio routing (which RX channel is subscribed to
/// which TX channel, on which device - see [`dante_control`]) to guess
/// `[[mapping]]` entries between the already-identified `devices`.
///
/// **Caveat, surfaced to the user in the written file's header rather
/// than hidden:** the resulting `channel` numbers are *Dante audio
/// channel numbers*. Whether that equals the preamp/headamp channel
/// number a vendor adapter addresses is a default-configuration
/// convention on most gear (1:1 local I/O order), not a protocol
/// guarantee - a console with customized Dante patching can break this
/// assumption. That's why this is opt-in (`--infer-mappings`) rather
/// than the default.
///
/// Only considers subscriptions between two devices both present in
/// `devices` - a subscription to/from anything else (e.g. a laptop's
/// Dante Virtual Soundcard) can't be bridged anyway, since there's no
/// adapter for it. Devices with no known ARC port, or that don't answer
/// a query, are silently skipped rather than failing the whole run.
pub async fn infer_mappings(devices: &[DeviceConfig], arc_ports: &HashMap<IpAddr, u16>) -> Vec<ChannelMapping> {
    let mut arc_addr_of: HashMap<&str, SocketAddr> = HashMap::new();
    for device in devices {
        let Some(ip) = device.address else { continue };
        if let Some(&port) = arc_ports.get(&ip) {
            arc_addr_of.insert(&device.id, SocketAddr::new(ip, port));
        }
    }

    let mut id_of_dante_name: HashMap<String, &str> = HashMap::new();
    for device in devices {
        let Some(&addr) = arc_addr_of.get(device.id.as_str()) else { continue };
        match dante_control::query_device_name(addr).await {
            Ok(Some(name)) => {
                id_of_dante_name.insert(name, &device.id);
            }
            Ok(None) => {}
            Err(e) => debug!(device = %device.id, error = %e, "failed to query Dante device name"),
        }
    }

    let mut tx_channels_of: HashMap<&str, Vec<dante_control::TxChannel>> = HashMap::new();
    let mut mappings = Vec::new();

    for device in devices {
        let Some(&dest_addr) = arc_addr_of.get(device.id.as_str()) else { continue };
        let rx_channels = match dante_control::query_rx_channels(dest_addr).await {
            Ok(channels) => channels,
            Err(e) => {
                debug!(device = %device.id, error = %e, "failed to query Dante RX channels");
                continue;
            }
        };

        for rx in rx_channels {
            let (Some(tx_device_name), Some(tx_channel_name)) = (&rx.tx_device_name, &rx.tx_channel_name) else {
                continue;
            };
            let Some(&source_id) = id_of_dante_name.get(tx_device_name) else {
                continue;
            };
            if source_id == device.id {
                continue;
            }

            if !tx_channels_of.contains_key(source_id) {
                let Some(&source_addr) = arc_addr_of.get(source_id) else { continue };
                let channels = dante_control::query_tx_channels(source_addr).await.unwrap_or_default();
                tx_channels_of.insert(source_id, channels);
            }
            let Some(tx_number) = tx_channels_of[source_id]
                .iter()
                .find(|c| c.name.as_deref() == Some(tx_channel_name.as_str()))
                .map(|c| c.number)
            else {
                debug!(source = source_id, tx_channel_name, "could not resolve TX channel name to a number");
                continue;
            };

            mappings.push(ChannelMapping {
                from: PreampAddress::new(source_id, tx_number),
                to: PreampAddress::new(device.id.clone(), rx.number),
                bidirectional: true,
            });
        }
    }

    mappings
}

#[derive(Serialize)]
struct DeviceListToml<'a> {
    #[serde(rename = "device")]
    devices: &'a [DeviceConfig],
    #[serde(rename = "mapping", skip_serializing_if = "<[ChannelMapping]>::is_empty")]
    mappings: &'a [ChannelMapping],
}

const MAPPING_SCAFFOLD_HEADER: &str = "\
# Auto-generated by `preamp-bridge init`.
#
# Add [[mapping]] entries below to link channels between devices - mapping
# intent can't be inferred automatically. Syntax:
#
# [[mapping]]
# from = { device = \"...\", channel = N }
# to   = { device = \"...\", channel = N }
# bidirectional = true

";

const INFERRED_MAPPING_HEADER: &str = "\
# The [[mapping]] entries below were INFERRED by observing live Dante
# audio routing (--infer-mappings) - which RX channel is patched from
# which TX channel/device - NOT read from any documented intent. This is
# a real signal, but the channel numbers here are Dante audio channel
# numbers, which are only conventionally (not guaranteed) the same as
# the preamp/headamp channel numbers each adapter addresses. Verify
# against each adapter's channel numbering (see its module doc comment)
# before trusting these, especially on consoles where Dante patching has
# been customized away from the default 1:1 local-I/O order.

";

fn write_config_file(path: &PathBuf, config: &Config) -> Result<()> {
    let header = if config.mappings.is_empty() {
        MAPPING_SCAFFOLD_HEADER
    } else {
        INFERRED_MAPPING_HEADER
    };
    let body = toml::to_string_pretty(&DeviceListToml {
        devices: &config.devices,
        mappings: &config.mappings,
    })
    .context("serializing generated config")?;
    std::fs::write(path, format!("{header}{body}")).with_context(|| format!("writing {}", path.display()))
}

/// Discovers Dante devices via mDNS, probes each for a known protocol, and
/// writes the result to `output`. Refuses to overwrite an existing file
/// unless `force` is set, since that file may hold hand-added mappings.
/// When `with_inferred_mappings` is set, also observes live Dante routing
/// to guess `[[mapping]]` entries - see [`infer_mappings`] for the caveat
/// that keeps this opt-in.
pub async fn run(output: PathBuf, timeout: Duration, force: bool, with_inferred_mappings: bool) -> Result<()> {
    if output.exists() && !force {
        bail!(
            "{} already exists - pass --force to overwrite (this does NOT preserve any \
             [[mapping]] entries you've added)",
            output.display()
        );
    }

    info!("discovering Dante devices on the network...");
    let dante_devices = dante_babelbox_discovery::discover(timeout).await?;
    let ips: Vec<IpAddr> = dante_devices.iter().flat_map(|d| d.addresses.iter().copied()).collect();
    info!(count = ips.len(), "found candidate Dante address(es), probing each for a known protocol...");

    let mut config = probe_and_build(ips, timeout).await;
    info!(count = config.devices.len(), "identified device(s)");

    if with_inferred_mappings {
        info!("observing live Dante routing to infer mappings...");
        let arc_ports = dante_babelbox_discovery::arc_ports(&dante_devices);
        config.mappings = infer_mappings(&config.devices, &arc_ports).await;
        info!(count = config.mappings.len(), "inferred mapping(s)");
    }

    write_config_file(&output, &config)?;
    if config.mappings.is_empty() {
        println!(
            "Wrote {} device(s) to {}. Add [[mapping]] entries by hand - see bridge.example.toml for the syntax.",
            config.devices.len(),
            output.display()
        );
    } else {
        println!(
            "Wrote {} device(s) and {} inferred mapping(s) to {}. Inferred mappings are a best guess from \
             observed Dante routing, not a guarantee - see the file's header comment before trusting them.",
            config.devices.len(),
            config.mappings.len(),
            output.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn probe_and_build_identifies_and_skips_correctly() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        // Nothing listening on any candidate port yet - expect no devices.
        let empty = probe_and_build(vec![ip], Duration::from_millis(150)).await;
        assert!(empty.devices.is_empty());

        // Now start a Wing-shaped mock and probe again - expect it found.
        let mock = UdpSocket::bind((ip, DEFAULT_WING_PORT)).await.unwrap();
        let server = tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let (len, from) = mock.recv_from(&mut buf).await.unwrap();
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
            if let rosc::OscPacket::Message(m) = packet {
                if m.addr == "/?" {
                    let reply = rosc::OscPacket::Message(rosc::OscMessage {
                        addr: "/?".to_string(),
                        args: vec![rosc::OscType::String(
                            "WING,127.0.0.1,PGM,ngc-full,NO_SERIAL,1.0".into(),
                        )],
                    });
                    let bytes = rosc::encoder::encode(&reply).unwrap();
                    mock.send_to(&bytes, from).await.unwrap();
                }
            }
        });

        let found = probe_and_build(vec![ip], Duration::from_secs(2)).await;
        assert_eq!(found.devices.len(), 1);
        assert_eq!(found.devices[0].kind, "osc-wing");
        assert_eq!(found.devices[0].port, Some(DEFAULT_WING_PORT));
        assert_eq!(found.devices[0].id, "osc-wing-1");

        server.abort();
    }

    #[test]
    fn written_config_round_trips_through_config_load() {
        let config = Config {
            devices: vec![
                DeviceConfig {
                    id: "osc-wing-1".to_string(),
                    kind: "osc-wing".into(),
                    address: Some("10.0.0.25".parse().unwrap()),
                    port: Some(DEFAULT_WING_PORT),
                    is_virtual: false,
                    channels: None,
                },
                DeviceConfig {
                    id: "dlive-tcp-15".to_string(),
                    kind: "dlive-tcp".into(),
                    address: Some("10.0.0.15".parse().unwrap()),
                    port: Some(DEFAULT_DLIVE_PORT),
                    is_virtual: false,
                    channels: None,
                },
            ],
            mappings: Vec::new(),
        };

        let path = std::env::temp_dir().join(format!("init-roundtrip-{}.toml", std::process::id()));
        write_config_file(&path, &config).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[device]]"), "expected array-of-tables syntax, got:\n{text}");

        let loaded = crate::config::Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.devices.len(), 2);
        assert_eq!(loaded.devices[0].id, "osc-wing-1");
        assert_eq!(loaded.devices[1].kind, "dlive-tcp");
    }

    #[test]
    fn dedupe_id_suffixes_on_collision() {
        let mut used = HashSet::new();
        assert_eq!(dedupe_id("osc-wing-1".to_string(), &mut used), "osc-wing-1");
        assert_eq!(dedupe_id("osc-wing-1".to_string(), &mut used), "osc-wing-1-2");
        assert_eq!(dedupe_id("osc-wing-1".to_string(), &mut used), "osc-wing-1-3");
    }

    #[test]
    fn written_config_with_mappings_uses_inferred_header_and_round_trips() {
        let config = Config {
            devices: vec![
                DeviceConfig {
                    id: "stagebox".to_string(),
                    kind: "dlive-tcp".into(),
                    address: Some("10.0.0.15".parse().unwrap()),
                    port: Some(DEFAULT_DLIVE_PORT),
                    is_virtual: false,
                    channels: None,
                },
                DeviceConfig {
                    id: "console".to_string(),
                    kind: "osc-x32".into(),
                    address: Some("10.0.0.20".parse().unwrap()),
                    port: Some(DEFAULT_X32_PORT),
                    is_virtual: false,
                    channels: None,
                },
            ],
            mappings: vec![ChannelMapping {
                from: PreampAddress::new("stagebox", 1),
                to: PreampAddress::new("console", 5),
                bidirectional: true,
            }],
        };

        let path = std::env::temp_dir().join(format!("init-inferred-roundtrip-{}.toml", std::process::id()));
        write_config_file(&path, &config).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("INFERRED"), "expected the inferred-mapping caveat header, got:\n{text}");
        assert!(text.contains("[[mapping]]"), "expected array-of-tables mapping syntax, got:\n{text}");

        let loaded = crate::config::Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.mappings.len(), 1);
        assert_eq!(loaded.mappings[0].from, PreampAddress::new("stagebox", 1));
        assert_eq!(loaded.mappings[0].to, PreampAddress::new("console", 5));
    }

    /// End-to-end: two mock UDP "Dante ARC" devices - a stagebox with an
    /// unpatched TX channel and a console whose RX channel 5 is
    /// subscribed to it - proving `infer_mappings` resolves the
    /// subscription's `tx_channel_name` string back to a channel number
    /// and cross-references `tx_device_name` against our own identified
    /// devices' Dante names.
    ///
    /// Uses `127.0.0.1` for one mock and `::1` for the other so the two
    /// devices have distinct `IpAddr` keys in `arc_ports` - `127.0.0.2`
    /// doesn't bind on every machine without an interface alias, but
    /// `::1` always does.
    #[tokio::test]
    async fn infer_mappings_resolves_a_cross_device_subscription() {
        let stagebox_socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let stagebox_addr = stagebox_socket.local_addr().unwrap();
        let console_socket = UdpSocket::bind(("::1", 0)).await.unwrap();
        let console_addr = console_socket.local_addr().unwrap();

        let stagebox_server = tokio::spawn(async move {
            // device-name, empty RX page (it has no subscriptions of its
            // own), TX friendly-name page, TX info page - in that order.
            for _ in 0..4 {
                let mut buf = [0u8; 512];
                let (_len, from) = stagebox_socket.recv_from(&mut buf).await.unwrap();
                let transaction_id = u16::from_be_bytes([buf[4], buf[5]]);
                let opcode = u16::from_be_bytes([buf[6], buf[7]]);

                let mut reply = vec![0u8; 10];
                reply[0..2].copy_from_slice(&0x27FFu16.to_be_bytes());
                reply[4..6].copy_from_slice(&transaction_id.to_be_bytes());
                reply[6..8].copy_from_slice(&opcode.to_be_bytes());

                match opcode {
                    0x1002 => reply.extend_from_slice(b"stagebox-1\x00"),
                    0x3000 => reply.extend_from_slice(&[0u8; 2]), // empty RX page
                    0x2010 => {
                        reply.extend_from_slice(&[0u8; 2]);
                        let strings_base = (reply.len() + 6) as u16;
                        reply.extend_from_slice(&[0x00, 0x00]);
                        reply.extend_from_slice(&1u16.to_be_bytes());
                        reply.extend_from_slice(&strings_base.to_be_bytes());
                        reply.extend_from_slice(b"Out-1\x00");
                    }
                    0x2000 => {
                        reply.extend_from_slice(&[0u8; 2]);
                        let strings_base = (reply.len() + 8) as u16;
                        reply.extend_from_slice(&1u16.to_be_bytes());
                        reply.extend_from_slice(&[0x00, 0x00]);
                        reply.extend_from_slice(&strings_base.to_be_bytes());
                        reply.extend_from_slice(b"Out-1-raw\x00");
                    }
                    other => panic!("unexpected opcode {other:#06x} sent to stagebox mock"),
                }
                stagebox_socket.send_to(&reply, from).await.unwrap();
            }
        });

        let console_server = tokio::spawn(async move {
            // device-name, then one RX page whose first record (channel 1 -
            // parse_rx_page requires each page's records to be sequential
            // starting at the page's first channel number, so a lone
            // record must claim channel 1, not an arbitrary channel like 5)
            // is subscribed to stagebox-1's "Out-1".
            for _ in 0..2 {
                let mut buf = [0u8; 512];
                let (_len, from) = console_socket.recv_from(&mut buf).await.unwrap();
                let transaction_id = u16::from_be_bytes([buf[4], buf[5]]);
                let opcode = u16::from_be_bytes([buf[6], buf[7]]);

                let mut reply = vec![0u8; 10];
                reply[0..2].copy_from_slice(&0x27FFu16.to_be_bytes());
                reply[4..6].copy_from_slice(&transaction_id.to_be_bytes());
                reply[6..8].copy_from_slice(&opcode.to_be_bytes());

                match opcode {
                    0x1002 => reply.extend_from_slice(b"console-1\x00"),
                    0x3000 => {
                        reply.extend_from_slice(&[0u8; 2]);
                        let strings_base = (reply.len() + 20) as u16;
                        let rx_name_ptr = strings_base;
                        let tx_channel_ptr = rx_name_ptr + 5; // after "In-1\0"
                        let tx_device_ptr = tx_channel_ptr + 6; // after "Out-1\0"
                        reply.extend_from_slice(&1u16.to_be_bytes());
                        reply.extend_from_slice(&[0u8; 4]);
                        reply.extend_from_slice(&tx_channel_ptr.to_be_bytes());
                        reply.extend_from_slice(&tx_device_ptr.to_be_bytes());
                        reply.extend_from_slice(&rx_name_ptr.to_be_bytes());
                        reply.extend_from_slice(&[0u8; 8]);
                        reply.extend_from_slice(b"In-1\x00");
                        reply.extend_from_slice(b"Out-1\x00");
                        reply.extend_from_slice(b"stagebox-1\x00");
                    }
                    other => panic!("unexpected opcode {other:#06x} sent to console mock"),
                }
                console_socket.send_to(&reply, from).await.unwrap();
            }
        });

        let devices = vec![
            DeviceConfig {
                id: "stagebox".to_string(),
                kind: "dlive-tcp".into(),
                address: Some(stagebox_addr.ip()),
                port: None,
                is_virtual: false,
                channels: None,
            },
            DeviceConfig {
                id: "console".to_string(),
                kind: "osc-x32".into(),
                address: Some(console_addr.ip()),
                port: None,
                is_virtual: false,
                channels: None,
            },
        ];
        let mut arc_ports = HashMap::new();
        arc_ports.insert(stagebox_addr.ip(), stagebox_addr.port());
        arc_ports.insert(console_addr.ip(), console_addr.port());

        let mappings = infer_mappings(&devices, &arc_ports).await;

        stagebox_server.await.unwrap();
        console_server.await.unwrap();

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].from, PreampAddress::new("stagebox", 1));
        assert_eq!(mappings[0].to, PreampAddress::new("console", 1));
        assert!(mappings[0].bidirectional);
    }

    /// A subscription to a device outside our identified `devices` set
    /// (e.g. a laptop's Dante Virtual Soundcard) can't be bridged - no
    /// adapter exists for it - so it must produce no mapping rather than
    /// a broken one.
    #[tokio::test]
    async fn infer_mappings_skips_subscriptions_to_unrecognized_devices() {
        let console_socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let console_addr = console_socket.local_addr().unwrap();

        let console_server = tokio::spawn(async move {
            for _ in 0..2 {
                let mut buf = [0u8; 512];
                let (_len, from) = console_socket.recv_from(&mut buf).await.unwrap();
                let transaction_id = u16::from_be_bytes([buf[4], buf[5]]);
                let opcode = u16::from_be_bytes([buf[6], buf[7]]);

                let mut reply = vec![0u8; 10];
                reply[0..2].copy_from_slice(&0x27FFu16.to_be_bytes());
                reply[4..6].copy_from_slice(&transaction_id.to_be_bytes());
                reply[6..8].copy_from_slice(&opcode.to_be_bytes());

                match opcode {
                    0x1002 => reply.extend_from_slice(b"console-1\x00"),
                    0x3000 => {
                        // channel 1, not an arbitrary number - parse_rx_page
                        // requires each page's records to be sequential
                        // starting at the page's first channel number.
                        reply.extend_from_slice(&[0u8; 2]);
                        let strings_base = (reply.len() + 20) as u16;
                        let tx_channel_ptr = strings_base;
                        let tx_device_ptr = tx_channel_ptr + 6; // after "Out-1\0"
                        reply.extend_from_slice(&1u16.to_be_bytes());
                        reply.extend_from_slice(&[0u8; 4]);
                        reply.extend_from_slice(&tx_channel_ptr.to_be_bytes());
                        reply.extend_from_slice(&tx_device_ptr.to_be_bytes());
                        reply.extend_from_slice(&0u16.to_be_bytes());
                        reply.extend_from_slice(&[0u8; 8]);
                        reply.extend_from_slice(b"Out-1\x00");
                        reply.extend_from_slice(b"someone-elses-mixer\x00");
                    }
                    other => panic!("unexpected opcode {other:#06x} sent to console mock"),
                }
                console_socket.send_to(&reply, from).await.unwrap();
            }
        });

        let devices = vec![DeviceConfig {
            id: "console".to_string(),
            kind: "osc-x32".into(),
            address: Some(console_addr.ip()),
            port: None,
            is_virtual: false,
            channels: None,
        }];
        let mut arc_ports = HashMap::new();
        arc_ports.insert(console_addr.ip(), console_addr.port());

        let mappings = infer_mappings(&devices, &arc_ports).await;
        console_server.await.unwrap();

        assert!(mappings.is_empty());
    }
}

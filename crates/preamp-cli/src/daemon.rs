use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use dante_babelbox_core::{channel_mapping, channel_scheme, PluginRegistry, Router};
use dante_babelbox_oca::OcaObjectDescriptor;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::Config;

/// Registers the two known-but-unimplemented kinds (`ah-midi`/`yamaha`)
/// with a clear explanatory error - so looking one of those up still
/// surfaces specific guidance rather than a generic "no plugin
/// registered" message. Every real device kind (`osc-x32`, `osc-wing`,
/// `ah-tcp`, `dlive-tcp`, `yamaha-dm3`) comes exclusively from a loaded
/// `cdylib` plugin now - `LegacyPreampShim`, the in-process fallback that
/// used to back the last four of those, has been retired now that all
/// five preamp adapters run through the same dylib path `osc-x32` proved
/// out first (see `crates/plugin-osc-*`).
pub fn build_registry(plugins_dir: &Path) -> Arc<PluginRegistry> {
    let registry = Arc::new(PluginRegistry::new());

    registry.register_static("ah-midi", |device| {
        bail!(
            "device '{}': Qu/SQ preamp control is not implemented - A&H's public SQ/Qu MIDI protocol \
             doc doesn't document preamp gain/pad/phantom messages at all (unlike dLive, which has its \
             own documented Socket-based preamp protocol - use 'dlive-tcp' for that), so this needs \
             real-hardware verification before it can be built, not guessing",
            device.id
        )
    });
    registry.register_static("yamaha", |device| {
        bail!(
            "device '{}': Yamaha adapter not implemented yet - blocked on packet captures from real hardware",
            device.id
        )
    });

    let loaded = registry.load_dylibs(plugins_dir);
    if loaded.is_empty() {
        info!(dir = %plugins_dir.display(), "no dylib plugins loaded");
    } else {
        info!(kinds = ?loaded, "loaded dylib plugins");
    }

    registry
}

/// Connects every configured (non-virtual) device, wires them through a
/// [`Router`] using the config's mappings, and runs until interrupted.
/// Virtual devices are skipped here entirely - they exist only for the
/// web UI's mapping purposes, not as something this loop can dial, but
/// still get a synthesized descriptor set (via [`channel_scheme`]) so
/// mappings referencing them resolve the same way a real device's would.
/// If `config_path` is given, mapping changes to that file take effect
/// live. If `web_bind` is given, the patch-bay web UI (device/mapping
/// CRUD, live for virtual devices and mappings - see
/// `dante_babelbox_preamp_web`'s module doc for what's still restart-only
/// in this phase) is served there alongside the bridge.
pub async fn run(
    cfg: Config,
    config_path: Option<PathBuf>,
    web_bind: Option<SocketAddr>,
    plugins_dir: PathBuf,
) -> Result<()> {
    let registry = build_registry(&plugins_dir);
    let router = Router::new(Vec::new());
    let device_configs = cfg.devices.clone();
    let mut descriptors: HashMap<String, Vec<OcaObjectDescriptor>> = HashMap::new();

    for device in &cfg.devices {
        if device.is_virtual {
            info!(device = %device.id, "skipping virtual device - not yet backed by an emulation adapter");
            descriptors.insert(
                device.id.clone(),
                channel_scheme::descriptors_for_channels(device.channel_count().unwrap_or(0)),
            );
            continue;
        }

        let mut adapter = registry.create(&device.kind, device)?;
        adapter
            .connect()
            .await
            .with_context(|| format!("connecting to device '{}' at {:?}", device.id, device.address))?;
        info!(device = %device.id, kind = %device.kind, address = ?device.address, "connected");
        descriptors.insert(device.id.clone(), adapter.describe());

        router.register_device(device.id.clone(), Arc::new(Mutex::new(adapter))).await;
    }

    apply_channel_mappings(&router, &cfg.mappings, &descriptors);

    if let Some(path) = config_path {
        let router = Arc::clone(&router);
        tokio::spawn(watch_and_apply_mappings(path, router, descriptors.clone()));
    }

    if let Some(bind) = web_bind {
        let state = dante_babelbox_preamp_web::PatchState {
            router: Arc::clone(&router),
            devices: dante_babelbox_preamp_web::DeviceRegistry::new(device_configs),
            registry: Arc::clone(&registry),
            descriptors: Arc::new(std::sync::RwLock::new(descriptors.clone())),
            channel_mappings: Arc::new(std::sync::RwLock::new(cfg.mappings.clone())),
        };
        tokio::spawn(async move {
            if let Err(e) = dante_babelbox_preamp_web::serve(bind, state).await {
                warn!(error = %e, "patch-bay web UI server stopped");
            }
        });
        info!(%bind, "patch-bay web UI available");
    }

    info!("bridge running - press Ctrl-C to stop");

    tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")?;
    info!("shutting down");

    Ok(())
}

/// Resolves every device+channel-level [`crate::config::Config::mappings`]
/// entry into the Router's OCA-object-level mappings, using each
/// referenced device's already-known descriptor set. A mapping
/// referencing an unknown device (or one whose descriptors couldn't
/// resolve any matching object - e.g. an out-of-range channel) is logged
/// and skipped rather than failing the whole run.
fn apply_channel_mappings(
    router: &Arc<Router>,
    mappings: &[dante_babelbox_core::ChannelMapping],
    descriptors: &HashMap<String, Vec<OcaObjectDescriptor>>,
) {
    for mapping in mappings {
        let (Some(from), Some(to)) =
            (descriptors.get(&mapping.from.device_id), descriptors.get(&mapping.to.device_id))
        else {
            warn!(from = %mapping.from.device_id, to = %mapping.to.device_id, "mapping references an unknown device");
            continue;
        };
        let resolved = channel_mapping::resolve(mapping, from, to);
        if resolved.is_empty() {
            warn!(
                from = %mapping.from.device_id, from_channel = mapping.from.channel,
                to = %mapping.to.device_id, to_channel = mapping.to.channel,
                "mapping resolved to no shared objects - check the channel numbers are in range"
            );
        }
        for m in resolved {
            router.add_mapping(m);
        }
    }
}

/// Applies mapping changes from `config::watch` to a running Router as
/// they arrive. The channel's initial value is the config already applied
/// at startup, so the first real update only appears after an edit -
/// `changed()` doesn't fire for the value the receiver was created with.
/// Re-resolves against the same descriptor snapshot taken at startup -
/// device connections don't change on a mapping-only hot-reload, so their
/// descriptors don't either.
async fn watch_and_apply_mappings(
    path: PathBuf,
    router: Arc<Router>,
    descriptors: HashMap<String, Vec<OcaObjectDescriptor>>,
) {
    let mut rx = match crate::config::watch(path) {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "failed to start config hot-reload watcher");
            return;
        }
    };

    while rx.changed().await.is_ok() {
        let mappings = rx.borrow().mappings.clone();
        info!(count = mappings.len(), "applying hot-reloaded mapping config");
        router.update_mappings(Vec::new());
        apply_channel_mappings(&router, &mappings, &descriptors);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dante_babelbox_core::{ChannelMapping, DeviceConfig, PreampAddress};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, UdpSocket};

    /// Where `cargo test` puts this workspace's build artifacts, including
    /// every plugin crate's `cdylib` - built as a side effect of building
    /// their `rlib` targets, which `[dev-dependencies]` below pulls in for
    /// this crate's own test run regardless of whether their Rust API is
    /// ever referenced from here (see `docs/plugin-development-guide.md`'s
    /// "Testing" section for the same pattern applied inside each plugin
    /// crate's own test suite).
    fn plugins_dir_for_tests() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
    }

    /// End-to-end: a real, dylib-loaded `DliveAdapter` and `Dm3Adapter`
    /// (via `crates/plugin-dlive-tcp`/`crates/plugin-yamaha-dm3`), wired
    /// through the Router by `daemon::run()`, against mock TCP/UDP peers
    /// standing in for real hardware. Proves the full path from config ->
    /// dylib-loaded plugin registry -> connect -> Router propagation
    /// works, not just the mocked `LocalAdapter` used in the core Router
    /// unit tests, or a single plugin's own isolated test.
    #[tokio::test]
    async fn bridges_phantom_change_from_dlive_to_dm3() {
        let dlive_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dlive_addr = dlive_listener.local_addr().unwrap();

        let dm3_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dm3_addr = dm3_socket.local_addr().unwrap();

        let cfg = Config {
            devices: vec![
                DeviceConfig {
                    id: "dlive".into(),
                    kind: "dlive-tcp".into(),
                    address: Some(dlive_addr.ip()),
                    port: Some(dlive_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
                DeviceConfig {
                    id: "dm3".into(),
                    kind: "yamaha-dm3".into(),
                    address: Some(dm3_addr.ip()),
                    port: Some(dm3_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
            ],
            mappings: vec![ChannelMapping {
                from: PreampAddress::new("dlive", 12),
                to: PreampAddress::new("dm3", 5),
                bidirectional: true,
            }],
        };

        let bridge = tokio::spawn(run(cfg, None, None, plugins_dir_for_tests()));

        let (mut dlive_socket, _) = tokio::time::timeout(Duration::from_secs(2), dlive_listener.accept())
            .await
            .expect("timed out waiting for bridge to connect to mock dLive device")
            .unwrap();

        // Simulate dLive spontaneously reporting 48V-on for Socket 0x0B
        // (channel 12), as it would after a physical/on-screen toggle.
        let mut msg = vec![0xF0u8, 0x00, 0x00, 0x1A, 0x50, 0x10, 0x01, 0x00];
        msg.extend_from_slice(&[0x00, 0x0B, 0x0B, 0x7F, 0xF7]); // opcode 0x0B = 48V status
        dlive_socket.write_all(&msg).await.unwrap();

        // The plugin propagates gain and phantom as separate OCA events,
        // so the Router sends both a set-gain and a set-48VOn message to
        // DM3 (in some order); we only care about the phantom one here.
        let mut buf = [0u8; 512];
        let mut found = false;
        for _ in 0..2 {
            let (len, _) = tokio::time::timeout(Duration::from_secs(2), dm3_socket.recv_from(&mut buf))
                .await
                .expect("timed out waiting for DM3 relay")
                .unwrap();
            let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).unwrap();
            let rosc::OscPacket::Message(m) = packet else {
                panic!("expected OSC message, got a bundle")
            };
            if m.addr == "/yosc:req/set/IO:Current/InCh/48VOn/5/1" {
                assert_eq!(m.args, vec![rosc::OscType::Int(1)]);
                found = true;
                break;
            }
        }
        assert!(found, "did not see a 48VOn relay to DM3 channel 5");

        bridge.abort();
    }

    /// `build_registry` statically registers exactly the two
    /// explained-but-unimplemented kinds now - every real device kind
    /// comes exclusively from a loaded dylib (see the module doc comment).
    #[test]
    fn build_registry_covers_the_expected_static_kinds() {
        let registry = build_registry(Path::new("/nonexistent-plugins-dir"));
        let mut kinds = registry.known_kinds();
        kinds.sort();
        assert_eq!(kinds, vec!["ah-midi", "yamaha"]);
    }

    /// With a real plugins directory, `build_registry` picks up every
    /// migrated kind's dylib alongside the two static ones - proof the
    /// loader genuinely finds and registers all five real vendor kinds,
    /// not just the pair still statically wired.
    #[test]
    fn build_registry_loads_every_migrated_kind_from_a_real_plugins_dir() {
        let registry = build_registry(&plugins_dir_for_tests());
        let mut kinds = registry.known_kinds();
        kinds.sort();
        assert_eq!(kinds, vec!["ah-midi", "ah-tcp", "dlive-tcp", "osc-wing", "osc-x32", "yamaha", "yamaha-dm3"]);
    }

    #[test]
    fn ah_midi_and_yamaha_fail_with_their_explanatory_message_not_a_generic_one() {
        let registry = build_registry(Path::new("/nonexistent-plugins-dir"));
        let device = |kind: &str| DeviceConfig {
            id: "d".into(),
            kind: kind.into(),
            address: Some("10.0.0.1".parse().unwrap()),
            port: None,
            is_virtual: false,
            channels: Some(8),
        };

        let Err(err) = registry.create("ah-midi", &device("ah-midi")) else { panic!("expected an error") };
        assert!(err.to_string().contains("Qu/SQ preamp control is not implemented"));

        let Err(err) = registry.create("yamaha", &device("yamaha")) else { panic!("expected an error") };
        assert!(err.to_string().contains("blocked on packet captures from real hardware"));
    }
}

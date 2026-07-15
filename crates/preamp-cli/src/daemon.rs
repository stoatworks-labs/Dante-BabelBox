use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use dante_babelbox_preamp_adapter_ah::{AhmAdapter, DliveAdapter};
use dante_babelbox_preamp_adapter_osc::{WingAdapter, X32Adapter};
use dante_babelbox_preamp_adapter_yamaha::Dm3Adapter;
use dante_babelbox_core::{DeviceAdapter, DeviceConfig, DeviceKind, Router};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::Config;
use crate::ports::{DEFAULT_AHM_PORT, DEFAULT_DLIVE_PORT, DEFAULT_DM3_PORT, DEFAULT_WING_PORT, DEFAULT_X32_PORT};

/// Builds (but does not connect) the adapter for a single real device.
/// Real-devices-only: callers must filter out `is_virtual` devices first
/// (a virtual device has no protocol to build an adapter for yet - it's
/// a placeholder for the not-yet-built emulation layer). Shared between
/// the startup loop below and the web management API's "add device"
/// handler, so both construct adapters identically.
pub fn build_adapter(device: &DeviceConfig) -> Result<Box<dyn DeviceAdapter>> {
    let ip = device
        .address
        .with_context(|| format!("device '{}' has no address", device.id))?;

    Ok(match device.kind {
        DeviceKind::OscX32 => {
            let addr = SocketAddr::new(ip, device.port.unwrap_or(DEFAULT_X32_PORT));
            Box::new(X32Adapter::new(device.id.clone(), addr))
        }
        DeviceKind::AhTcp => {
            let addr = SocketAddr::new(ip, device.port.unwrap_or(DEFAULT_AHM_PORT));
            Box::new(AhmAdapter::new(device.id.clone(), addr))
        }
        DeviceKind::OscWing => {
            let addr = SocketAddr::new(ip, device.port.unwrap_or(DEFAULT_WING_PORT));
            Box::new(WingAdapter::new(device.id.clone(), addr))
        }
        DeviceKind::DliveTcp => {
            let addr = SocketAddr::new(ip, device.port.unwrap_or(DEFAULT_DLIVE_PORT));
            Box::new(DliveAdapter::new(device.id.clone(), addr))
        }
        DeviceKind::YamahaDm3 => {
            let addr = SocketAddr::new(ip, device.port.unwrap_or(DEFAULT_DM3_PORT));
            Box::new(Dm3Adapter::new(device.id.clone(), addr))
        }
        DeviceKind::AhMidi => bail!(
            "device '{}': Qu/SQ preamp control is not implemented - A&H's public SQ/Qu MIDI protocol \
             doc doesn't document preamp gain/pad/phantom messages at all (unlike dLive, which has its \
             own documented Socket-based preamp protocol - use 'dlive-tcp' for that), so this needs \
             real-hardware verification before it can be built, not guessing",
            device.id
        ),
        DeviceKind::Yamaha => bail!(
            "device '{}': Yamaha adapter not implemented yet - blocked on packet captures from real hardware",
            device.id
        ),
    })
}

/// Connects every configured (non-virtual) device, wires them through a
/// [`Router`] using the config's mappings, and runs until interrupted.
/// Virtual devices are skipped here entirely - they exist only for the
/// web UI's mapping purposes, not as something this loop can dial. If
/// `config_path` is given, mapping changes to that file take effect live.
/// If `web_bind` is given, the patch-bay web UI (device/mapping CRUD,
/// live for virtual devices and mappings - see `dante_babelbox_preamp_web`'s
/// module doc for what's still restart-only in this phase) is served
/// there alongside the bridge.
pub async fn run(cfg: Config, config_path: Option<PathBuf>, web_bind: Option<SocketAddr>) -> Result<()> {
    let router = Router::new(cfg.mappings);
    let device_configs = cfg.devices.clone();

    for device in &cfg.devices {
        if device.is_virtual {
            info!(device = %device.id, "skipping virtual device - not yet backed by an emulation adapter");
            continue;
        }

        let mut adapter = build_adapter(device)?;
        adapter
            .connect()
            .await
            .with_context(|| format!("connecting to device '{}' at {:?}", device.id, device.address))?;
        info!(device = %device.id, kind = ?device.kind, address = ?device.address, "connected");

        router.register_device(device.id.clone(), Arc::new(Mutex::new(adapter))).await;
    }

    if let Some(path) = config_path {
        let router = Arc::clone(&router);
        tokio::spawn(watch_and_apply_mappings(path, router));
    }

    if let Some(bind) = web_bind {
        let state = dante_babelbox_preamp_web::PatchState {
            router: Arc::clone(&router),
            devices: dante_babelbox_preamp_web::DeviceRegistry::new(device_configs),
            build_adapter: Arc::new(build_adapter),
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

/// Applies mapping changes from `config::watch` to a running Router as
/// they arrive. The channel's initial value is the config already applied
/// at startup, so the first real update only appears after an edit -
/// `changed()` doesn't fire for the value the receiver was created with.
async fn watch_and_apply_mappings(path: PathBuf, router: Arc<Router>) {
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
        router.update_mappings(mappings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dante_babelbox_core::{DeviceConfig, Mapping, PreampAddress};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    /// End-to-end: a real X32Adapter and a real AhmAdapter, wired through
    /// the Router by daemon::run(), against mock UDP/TCP peers standing in
    /// for an actual X32 console and AHM rack. Proves the full path from
    /// config -> adapter construction -> connect -> Router propagation
    /// works across two different vendor protocols, not just the mocked
    /// DeviceAdapter used in the core Router unit test.
    #[tokio::test]
    async fn bridges_gain_change_from_x32_to_ahm() {
        let ahm_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ahm_addr = ahm_listener.local_addr().unwrap();

        let x32_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let x32_addr = x32_socket.local_addr().unwrap();

        let cfg = Config {
            devices: vec![
                DeviceConfig {
                    id: "x32".into(),
                    kind: DeviceKind::OscX32,
                    address: Some(x32_addr.ip()),
                    port: Some(x32_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
                DeviceConfig {
                    id: "ahm".into(),
                    kind: DeviceKind::AhTcp,
                    address: Some(ahm_addr.ip()),
                    port: Some(ahm_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
            ],
            mappings: vec![Mapping {
                from: PreampAddress::new("x32", 3),
                to: PreampAddress::new("ahm", 7),
                bidirectional: true,
            }],
        };

        let bridge = tokio::spawn(run(cfg, None, None));

        let (mut ahm_socket, _) = tokio::time::timeout(Duration::from_secs(2), ahm_listener.accept())
            .await
            .expect("timed out waiting for bridge to connect to mock AHM device")
            .unwrap();

        // First inbound packet is the X32 adapter's /xremote heartbeat,
        // which tells us which ephemeral local port it's using.
        let bridge_client_addr = {
            let mut buf = [0u8; 512];
            let (_, from) = tokio::time::timeout(Duration::from_secs(2), x32_socket.recv_from(&mut buf))
                .await
                .expect("timed out waiting for /xremote heartbeat")
                .unwrap();
            from
        };

        // Simulate the X32 console spontaneously reporting a gain change on
        // channel 3, as it would after a physical/on-screen knob turn.
        let packet = rosc::encoder::encode(&rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/headamp/03/gain".to_string(),
            args: vec![rosc::OscType::Float(20.0)],
        }))
        .unwrap();
        x32_socket.send_to(&packet, bridge_client_addr).await.unwrap();

        // Expect the Router to relay it to the mapped AHM channel 7
        // (CH=06) as a set-gain NRPN sequence.
        let mut buf = [0u8; 9];
        tokio::time::timeout(Duration::from_secs(2), ahm_socket.read_exact(&mut buf))
            .await
            .expect("timed out waiting for AHM relay")
            .unwrap();

        assert_eq!(buf[0], 0xB0);
        assert_eq!(buf[1], 0x63);
        assert_eq!(buf[2], 0x06, "expected channel index 6 (channel 7)");
        assert_eq!(buf[3], 0xB0);
        assert_eq!(buf[4], 0x62);
        assert_eq!(buf[5], 0x19, "expected preamp gain parameter id");
        assert_eq!(buf[6], 0xB0);
        assert_eq!(buf[7], 0x06);

        bridge.abort();
    }

    /// Same shape as the X32<->AHM test above, but exercising the two
    /// newest adapters (Allen & Heath dLive and Yamaha DM3) to prove
    /// genuine three-vendor interoperability, not just a single pair.
    /// dLive (TCP) is used as the source since we already hold its
    /// accepted socket; DM3 (UDP, connectionless) is used as the
    /// receive-only destination so the test never needs to learn the
    /// bridge's ephemeral outbound UDP port.
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
                    kind: DeviceKind::DliveTcp,
                    address: Some(dlive_addr.ip()),
                    port: Some(dlive_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
                DeviceConfig {
                    id: "dm3".into(),
                    kind: DeviceKind::YamahaDm3,
                    address: Some(dm3_addr.ip()),
                    port: Some(dm3_addr.port()),
                    is_virtual: false,
                    channels: None,
                },
            ],
            mappings: vec![Mapping {
                from: PreampAddress::new("dlive", 12),
                to: PreampAddress::new("dm3", 5),
                bidirectional: true,
            }],
        };

        let bridge = tokio::spawn(run(cfg, None, None));

        let (mut dlive_socket, _) = tokio::time::timeout(Duration::from_secs(2), dlive_listener.accept())
            .await
            .expect("timed out waiting for bridge to connect to mock dLive device")
            .unwrap();

        // Simulate dLive spontaneously reporting 48V-on for Socket 0x0B
        // (channel 12), as it would after a physical/on-screen toggle.
        let mut msg = vec![0xF0u8, 0x00, 0x00, 0x1A, 0x50, 0x10, 0x01, 0x00];
        msg.extend_from_slice(&[0x00, 0x0B, 0x0B, 0x7F, 0xF7]); // opcode 0x0B = 48V status
        dlive_socket.write_all(&msg).await.unwrap();

        // The Router propagates a full PreampState, so it sends both a
        // set-gain and a set-48VOn message to DM3 (in that order); we
        // only care about the phantom one here.
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
}

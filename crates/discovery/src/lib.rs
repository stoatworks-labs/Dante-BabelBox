use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::debug;

pub mod dante_control;

pub const DANTE_ARC_SERVICE: &str = "_netaudio-arc._udp.local.";
pub const DANTE_CHAN_SERVICE: &str = "_netaudio-chan._udp.local.";

/// A device found via Dante's own mDNS/DNS-SD advertisement. This only
/// confirms "a Dante device exists at this address" - Dante carries no
/// control-plane information, so vendor/model/preamp-protocol identity is
/// unknown until an adapter's own `identify()` handshake succeeds against
/// one of these addresses.
#[derive(Debug, Clone)]
pub struct DanteDevice {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
}

pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<DanteDevice>> {
    let daemon = ServiceDaemon::new()?;
    let mut found: Vec<DanteDevice> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    for service_type in [DANTE_ARC_SERVICE, DANTE_CHAN_SERVICE] {
        let receiver = daemon.browse(service_type)?;
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let event = tokio::time::timeout(remaining, receiver.recv_async()).await;
            match event {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    let name = info.get_fullname().to_string();
                    if seen_names.insert(name.clone()) {
                        debug!(%name, "resolved Dante mDNS service");
                        found.push(DanteDevice {
                            name,
                            addresses: info.get_addresses().iter().copied().collect(),
                            port: info.get_port(),
                        });
                    }
                }
                Ok(Ok(_other)) => continue,
                Ok(Err(_recv_error)) => break,
                Err(_elapsed) => break,
            }
        }

        let _ = daemon.stop_browse(service_type);
    }

    Ok(found)
}

/// Filters `discover()`'s output down to just the ARC (control) service
/// instances, mapping each of their addresses to that service's port.
///
/// `discover()` browses both `_netaudio-arc._udp` (control) and
/// `_netaudio-chan._udp` (a different service entirely) into one
/// undifferentiated `Vec<DanteDevice>`, so `DanteDevice.port` alone
/// doesn't tell you which service it came from - this does, by checking
/// each entry's `name` (the mDNS instance name, which includes the
/// service type as its suffix, e.g. `"Foo._netaudio-arc._udp.local."`).
/// Needed because [`dante_control`]'s queries must target the ARC port
/// specifically.
pub fn arc_ports(devices: &[DanteDevice]) -> HashMap<IpAddr, u16> {
    let mut ports = HashMap::new();
    for device in devices {
        if !device.name.contains("_netaudio-arc") {
            continue;
        }
        for address in &device.addresses {
            ports.insert(*address, device.port);
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_ports_keeps_only_arc_service_entries() {
        let devices = vec![
            DanteDevice {
                name: "Mixer._netaudio-arc._udp.local.".to_string(),
                addresses: vec!["10.0.0.5".parse().unwrap()],
                port: 4440,
            },
            DanteDevice {
                name: "Mixer._netaudio-chan._udp.local.".to_string(),
                addresses: vec!["10.0.0.5".parse().unwrap()],
                port: 4455,
            },
            DanteDevice {
                name: "Stagebox._netaudio-chan._udp.local.".to_string(),
                addresses: vec!["10.0.0.6".parse().unwrap()],
                port: 4460,
            },
        ];

        let ports = arc_ports(&devices);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports.get(&"10.0.0.5".parse().unwrap()), Some(&4440));
        assert_eq!(ports.get(&"10.0.0.6".parse().unwrap()), None);
    }
}

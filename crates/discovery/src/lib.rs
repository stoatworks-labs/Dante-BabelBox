use std::collections::HashSet;
use std::net::IpAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::debug;

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

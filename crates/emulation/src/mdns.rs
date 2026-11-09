//! The mDNS records a Dante device advertises.
//!
//! Read off the Rio3224-D2 in the reference capture, which published:
//!
//! | Service | Instance | Port | TXT |
//! |---|---|---|---|
//! | `_netaudio-arc._udp` (+ `Yamaha._sub`) | device name | 4440 | `arcp_vers`, `arcp_min`, `router_vers`, `router_info`, `mf`, `model`, `router_debug` |
//! | `_netaudio-cmc._udp` (+ `Yamaha._sub`) | device name | 8800 | `id` (ConMon device id), `process`, `cmcp_vers`, `cmcp_min`, `server_vers`, `channels` |
//! | `_netaudio-dbc._udp` | device name | 4455 | none |
//! | `_netaudio-chan._udp` | `NN@device name`, one per transmit channel | 4455 | `txtvers`, `dbcp1`, `id`, `dbcp`, `rate`, `en`, `pcm`, `enc`, `latency_ns`, `fpp`, `nchan` |
//!
//! All four point at the Dante module's own host name
//! (`BROOKLYN3-25df04.local`).
//!
//! The version strings are the Rio's and are sent as captured. `dbc` and
//! `chan` point at the port a real device sets audio flows up on. Nothing
//! listens there: this device has no audio, so a subscription to one of its
//! channels resolves the name and then stays unconnected.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

use crate::arc::module_host_name;
use crate::identity::DeviceIdentity;
use crate::persona::Persona;

/// Port a real device takes flow requests on. Advertised, never bound.
pub const DBC_PORT: u16 = 4455;

#[derive(Debug, thiserror::Error)]
#[error("mDNS advertisement failed: {0}")]
pub struct AdvertiseError(String);

impl From<mdns_sd::Error> for AdvertiseError {
    fn from(e: mdns_sd::Error) -> Self {
        Self(e.to_string())
    }
}

/// One service record, before it is handed to the mDNS responder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Service type, with a `<maker>._sub.` prefix where the device
    /// registers a subtype.
    pub service: String,
    pub instance: String,
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

/// Every record the device advertises. Kept apart from the responder so the
/// set can be checked against the capture without touching the network.
pub fn records(identity: &DeviceIdentity, persona: &Persona, arc_port: u16, cmc_port: u16) -> Vec<Record> {
    fn txt(pairs: &[(&str, String)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }
    let device_id: String = identity.eui64().iter().map(|b| format!("{b:02x}")).collect();
    let maker = &persona.manufacturer_id;

    let mut out = vec![
        Record {
            service: format!("{maker}._sub._netaudio-arc._udp.local."),
            instance: identity.name.clone(),
            port: arc_port,
            txt: txt(&[
                ("arcp_vers", "2.8.9".into()),
                ("arcp_min", "0.2.4".into()),
                ("router_vers", "4.3.0".into()),
                ("router_info", "Audinate DCM".into()),
                ("mf", maker.clone()),
                ("model", persona.model_id.clone()),
                ("router_debug", "Build :800".into()),
            ]),
        },
        Record {
            service: format!("{maker}._sub._netaudio-cmc._udp.local."),
            instance: identity.name.clone(),
            port: cmc_port,
            txt: txt(&[
                ("id", device_id),
                ("process", "0".into()),
                ("cmcp_vers", "1.2.0".into()),
                ("cmcp_min", "1.0.0".into()),
                ("server_vers", "4.0.5".into()),
                ("channels", "0x6000017f".into()),
            ]),
        },
        Record {
            service: "_netaudio-dbc._udp.local.".into(),
            instance: identity.name.clone(),
            port: DBC_PORT,
            txt: Vec::new(),
        },
    ];
    for channel in 1..=persona.tx_channels {
        out.push(Record {
            service: "_netaudio-chan._udp.local.".into(),
            instance: format!("{}@{}", persona.tx_channel_name(channel), identity.name),
            port: DBC_PORT,
            txt: txt(&[
                ("txtvers", "2".into()),
                ("dbcp1", "0x1200".into()),
                ("id", channel.to_string()),
                ("dbcp", "0x1004".into()),
                ("rate", persona.sample_rate.to_string()),
                ("en", "24".into()),
                ("pcm", "3 c".into()),
                ("enc", "24".into()),
                ("latency_ns", "1000000".into()),
                ("fpp", "16,2".into()),
                ("nchan", "64".into()),
            ]),
        });
    }
    out
}

/// A running advertisement. Dropping it withdraws the records.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullnames: Vec<String>,
}

impl Advertisement {
    /// Advertises the device on the interface that holds `identity.ip`, and
    /// only there: a Dante network is one interface, and announcing a
    /// stagebox on the office Wi-Fi as well helps nobody.
    pub fn start(
        identity: &DeviceIdentity,
        persona: &Persona,
        arc_port: u16,
        cmc_port: u16,
    ) -> Result<Self, AdvertiseError> {
        let daemon = ServiceDaemon::new()?;
        daemon.disable_interface(IfKind::All)?;
        daemon.enable_interface(IfKind::Addr(IpAddr::V4(identity.ip)))?;
        // So a controller on this same machine can find the device too.
        daemon.set_multicast_loop_v4(true)?;

        let host = format!("{}.local.", module_host_name(identity, persona));
        let mut fullnames = Vec::new();
        for record in records(identity, persona, arc_port, cmc_port) {
            let txt: HashMap<String, String> = record.txt.into_iter().collect();
            let info = ServiceInfo::new(
                &record.service,
                &record.instance,
                &host,
                IpAddr::V4(identity.ip),
                record.port,
                txt,
            )?;
            fullnames.push(info.get_fullname().to_string());
            daemon.register(info)?;
        }
        Ok(Self { daemon, fullnames })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        // Goodbye packets, so controllers drop the device now rather than
        // when its records time out. The responder sends them on its own
        // thread, so it is given a moment to finish before it is stopped.
        let pending: Vec<_> =
            self.fullnames.iter().filter_map(|fullname| self.daemon.unregister(fullname).ok()).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        for receiver in pending {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if receiver.recv_timeout(remaining).is_err() {
                break;
            }
        }
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rio() -> (DeviceIdentity, Persona) {
        (
            DeviceIdentity {
                name: "Y004-Yamaha-Rio3224-D2-25df04".into(),
                mac: [0x00, 0x1d, 0xc1, 0x25, 0xdf, 0x04],
                ip: "169.254.193.229".parse().unwrap(),
            },
            Persona::yamaha_rio3224_d2(),
        )
    }

    fn value<'a>(record: &'a Record, key: &str) -> &'a str {
        &record.txt.iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("no {key}")).1
    }

    #[test]
    fn the_record_set_is_the_one_the_rio_published() {
        let (identity, persona) = rio();
        let records = records(&identity, &persona, 4440, 8800);
        assert_eq!(records.len(), 3 + 32, "arc, cmc, dbc and 32 channels");

        let arc = &records[0];
        assert_eq!(arc.service, "Yamaha._sub._netaudio-arc._udp.local.");
        assert_eq!(arc.instance, "Y004-Yamaha-Rio3224-D2-25df04");
        assert_eq!(arc.port, 4440);
        assert_eq!(value(arc, "mf"), "Yamaha");
        assert_eq!(value(arc, "model"), "8R19");
        assert_eq!(value(arc, "arcp_vers"), "2.8.9");

        let cmc = &records[1];
        assert_eq!(cmc.service, "Yamaha._sub._netaudio-cmc._udp.local.");
        assert_eq!(cmc.port, 8800);
        assert_eq!(value(cmc, "id"), "001dc1fffe25df04");
        assert_eq!(value(cmc, "channels"), "0x6000017f");

        assert_eq!(records[2].service, "_netaudio-dbc._udp.local.");
        assert!(records[2].txt.is_empty());

        let first = &records[3];
        assert_eq!(first.instance, "01@Y004-Yamaha-Rio3224-D2-25df04");
        assert_eq!(first.port, DBC_PORT);
        assert_eq!(value(first, "id"), "1");
        assert_eq!(value(first, "rate"), "48000");
        assert_eq!(records[34].instance, "32@Y004-Yamaha-Rio3224-D2-25df04");
        assert_eq!(value(&records[34], "id"), "32");
    }

    #[test]
    fn the_ports_advertised_are_the_ports_bound() {
        let (identity, persona) = rio();
        let records = records(&identity, &persona, 51000, 51001);
        assert_eq!(records[0].port, 51000);
        assert_eq!(records[1].port, 51001);
    }
}

//! `MicAdapter` for Sennheiser EW-DX EM 2 / EM 2 Dante / EM 4 Dante
//! receivers, built from Sennheiser's official "SSC Developer's Guide for
//! EW-DX EM" (Firmware 3.0.x, Publ. 03/2024,
//! <https://www.sennheiser.com/globalassets/digizuite/47000-en-ew-dx_sound_control_protocol_3_0_x.pdf>).
//!
//! Wire format: JSON over **UDP** (not the newer HTTPS+SSE "SSCv2" used by
//! some other Sennheiser product lines - EW-DX's own doc states "The SSC
//! Server implemented for EW-DX devices supports only UDP/IP as transport
//! protocol"). One UDP datagram carries exactly one JSON object. Default
//! port 45 (normally discovered via mDNS service type `_ssc._udp`, per
//! the spec - this adapter, like every other one here, is configured by
//! IP/port directly rather than depending on that discovery working).
//!
//! Every SSC address is a JSON path, e.g. `/rx1/mute` <-> `{"rx1":{"mute":
//! ...}}`. Sending a leaf's value as JSON `null` is a "get"; the server
//! replies with the same shape carrying the current value. Multiple
//! leaves can be combined in one message/reply.
//!
//! **Subscriptions carry the initial value for free**: per the spec, a
//! subscribe request (`/osc/state/subscribe`) immediately returns "the
//! result of calling the subscribed SSC Methods... with null-argument" as
//! its first notification, then keeps pushing updates on change. So
//! `get_state` only needs to *subscribe* once per channel (lazily, since
//! `connect()` has no channel argument to subscribe up front) rather than
//! separately GET-then-subscribe. Subscriptions default to a 10s lifetime
//! unless the request specifies a longer one; this adapter requests
//! `"lifetime": 3600` (1 hour) to avoid needing a renewal heartbeat for
//! any reasonably-lengthed monitoring session - a `mic-monitor watch` run
//! longer than that would need re-subscription, which isn't implemented.
//!
//! Fields implemented here: `/rxN/mute` (get/set), `/rxN/frequency`,
//! `/m/rxN/{rssi,rsqi,divi,af}` (RF level in dBm, RF quality %, antenna
//! diversity, and **genuinely calibrated dBFS** audio level - unlike
//! Shure's uncalibrated meter, this one's spec states units explicitly),
//! and `/mates/txN/battery/{gauge,lifetime}` for the paired transmitter's
//! battery. The transmitter index always matches the receiver channel
//! index in every worked example in the doc (`rx1` mates with `mates/
//! tx1`, `rx2` with `mates/tx2`, ...), so this adapter uses the channel
//! number directly for both - confirmed by example, not assumed.
//! `/device/identity/{vendor,product,version}` backs `identify()` -
//! unlike Shure, EW-DX genuinely documents a device-identity query.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dante_babelbox_mic_core::{
    AdapterError, AdapterResult, AntennaDiversity, DeviceInfo, MicAddress, MicAdapter, MicEvent, MicState,
};
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};

const SUBSCRIBE_LIFETIME_SECS: u64 = 3600;

#[derive(Debug, Clone)]
struct Identity {
    vendor: String,
    product: String,
}

pub struct SennheiserAdapter {
    id: Arc<str>,
    remote: SocketAddr,
    socket: Option<Arc<UdpSocket>>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
    identity: Arc<Mutex<Option<Identity>>>,
}

impl SennheiserAdapter {
    pub fn new(id: impl Into<Arc<str>>, remote: SocketAddr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            remote,
            socket: None,
            tx,
            state: Arc::new(Mutex::new(HashMap::new())),
            identity: Arc::new(Mutex::new(None)),
        }
    }

    async fn send(&self, value: &Value) -> AdapterResult<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;
        let bytes = value.to_string();
        socket
            .send(bytes.as_bytes())
            .await
            .map(|_| ())
            .map_err(|e| AdapterError::Connection(e.to_string()))
    }

    async fn subscribe_channel(&self, channel: u16) -> AdapterResult<()> {
        let rx = format!("rx{channel}");
        let tx = format!("tx{channel}");
        let request = json!({
            "osc": { "state": { "subscribe": [{
                "#": { "lifetime": SUBSCRIBE_LIFETIME_SECS },
                rx.clone(): { "mute": null, "frequency": null },
                "m": { rx: { "rssi": null, "rsqi": null, "divi": null, "af": null } },
                "mates": { tx: { "battery": { "gauge": null, "lifetime": null } } }
            }]}}
        });
        self.send(&request).await
    }
}

fn empty_state() -> MicState {
    MicState {
        battery_percent: None,
        battery_minutes_remaining: None,
        rf_level_dbm: None,
        rf_quality_percent: None,
        audio_level_dbfs: None,
        muted: false,
        frequency_mhz: None,
        antenna: None,
    }
}

fn antenna_from_divi(n: u64) -> Option<AntennaDiversity> {
    match n {
        0 => Some(AntennaDiversity::Inactive),
        1 => Some(AntennaDiversity::A),
        2 => Some(AntennaDiversity::B),
        _ => None,
    }
}

fn parse_indexed(key: &str, prefix: &str) -> Option<u16> {
    key.strip_prefix(prefix)?.parse().ok()
}

/// One field found while walking a reply/notification, paired with the
/// receiver channel it applies to (or `None` for the device-wide identity
/// fields). Pure and synchronous so it's unit-testable without a socket -
/// mirrors `dante_babelbox_mic_adapter_shure::shure::parse_message`.
#[derive(Debug, PartialEq)]
enum Update {
    Mute { channel: u16, muted: bool },
    FrequencyMhz { channel: u16, mhz: f64 },
    Rssi { channel: u16, dbm: f32 },
    Rsqi { channel: u16, percent: u8 },
    Divi { channel: u16, antenna: Option<AntennaDiversity> },
    Af { channel: u16, dbfs: f32 },
    BatteryGauge { channel: u16, percent: Option<u8> },
    BatteryLifetime { channel: u16, minutes: Option<u16> },
    Identity { vendor: Option<String>, product: Option<String> },
}

fn extract_updates(root: &Value) -> Vec<Update> {
    let mut out = Vec::new();
    let Some(obj) = root.as_object() else {
        return out;
    };

    for (key, val) in obj {
        if let Some(channel) = parse_indexed(key, "rx") {
            if let Some(muted) = val.get("mute").and_then(Value::as_bool) {
                out.push(Update::Mute { channel, muted });
            }
            if let Some(khz) = val.get("frequency").and_then(Value::as_f64) {
                out.push(Update::FrequencyMhz { channel, mhz: khz / 1000.0 });
            }
        } else if key == "m" {
            if let Some(m) = val.as_object() {
                for (rx_key, rx_val) in m {
                    let Some(channel) = parse_indexed(rx_key, "rx") else { continue };
                    if let Some(dbm) = rx_val.get("rssi").and_then(Value::as_f64) {
                        out.push(Update::Rssi { channel, dbm: dbm as f32 });
                    }
                    if let Some(pct) = rx_val.get("rsqi").and_then(Value::as_u64) {
                        out.push(Update::Rsqi { channel, percent: pct as u8 });
                    }
                    if let Some(n) = rx_val.get("divi").and_then(Value::as_u64) {
                        out.push(Update::Divi { channel, antenna: antenna_from_divi(n) });
                    }
                    if let Some(dbfs) = rx_val.get("af").and_then(Value::as_f64) {
                        out.push(Update::Af { channel, dbfs: dbfs as f32 });
                    }
                }
            }
        } else if key == "mates" {
            if let Some(mates) = val.as_object() {
                for (tx_key, tx_val) in mates {
                    let Some(channel) = parse_indexed(tx_key, "tx") else { continue };
                    if let Some(battery) = tx_val.get("battery") {
                        // A missing/non-numeric field (including the
                        // documented error-424 "TX absent" case) simply
                        // stays None here rather than being guessed at -
                        // the exact SSC error-reply shape isn't confirmed
                        // in the portion of the spec this was built from.
                        out.push(Update::BatteryGauge {
                            channel,
                            percent: battery.get("gauge").and_then(Value::as_u64).map(|v| v as u8),
                        });
                        out.push(Update::BatteryLifetime {
                            channel,
                            minutes: battery.get("lifetime").and_then(Value::as_u64).map(|v| v as u16),
                        });
                    }
                }
            }
        } else if key == "device" {
            if let Some(identity) = val.get("identity") {
                let vendor = identity.get("vendor").and_then(Value::as_str).map(str::to_string);
                let product = identity.get("product").and_then(Value::as_str).map(str::to_string);
                if vendor.is_some() || product.is_some() {
                    out.push(Update::Identity { vendor, product });
                }
            }
        }
    }

    out
}

#[async_trait]
impl MicAdapter for SennheiserAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        socket
            .connect(self.remote)
            .await
            .map_err(|e| AdapterError::Connection(e.to_string()))?;
        let socket = Arc::new(socket);
        self.socket = Some(Arc::clone(&socket));

        spawn_receive_loop(socket, Arc::clone(&self.id), self.tx.clone(), Arc::clone(&self.state), Arc::clone(&self.identity));

        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        if let Some(identity) = self.identity.lock().await.clone() {
            return Ok(DeviceInfo {
                vendor: identity.vendor,
                model: identity.product,
                address: self.remote.ip(),
            });
        }

        self.send(&json!({"device": {"identity": {"vendor": null, "product": null}}}))
            .await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(identity) = self.identity.lock().await.clone() {
                return Ok(DeviceInfo {
                    vendor: identity.vendor,
                    model: identity.product,
                    address: self.remote.ip(),
                });
            }
        }
        Err(AdapterError::Protocol("no reply for device identity".into()))
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<MicState> {
        if let Some(state) = self.state.lock().await.get(&channel) {
            return Ok(*state);
        }
        // Subscribing delivers the current values as its first
        // notification, so this alone primes the cache - no separate GET.
        self.subscribe_channel(channel).await?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(state) = self.state.lock().await.get(&channel) {
                return Ok(*state);
            }
        }
        Err(AdapterError::Protocol(format!("no reply for channel {channel} mic state")))
    }

    async fn set_mute(&mut self, channel: u16, muted: bool) -> AdapterResult<()> {
        let rx = format!("rx{channel}");
        self.send(&json!({ rx: { "mute": muted } })).await
    }

    fn subscribe(&self) -> broadcast::Receiver<MicEvent> {
        self.tx.subscribe()
    }
}

fn spawn_receive_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    tx: broadcast::Sender<MicEvent>,
    state: Arc<Mutex<HashMap<u16, MicState>>>,
    identity: Arc<Mutex<Option<Identity>>>,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            let len = match socket.recv(&mut buf).await {
                Ok(len) => len,
                Err(e) => {
                    warn!(device = %id, error = %e, "Sennheiser UDP read failed, stopping receive loop");
                    return;
                }
            };
            let Ok(root) = serde_json::from_slice::<Value>(&buf[..len]) else {
                debug!(device = %id, "dropped malformed SSC datagram");
                continue;
            };

            let mut touched_channels: Vec<u16> = Vec::new();
            let mut identity_updated = false;

            for update in extract_updates(&root) {
                if let Update::Identity { vendor, product } = update {
                    let mut guard = identity.lock().await;
                    let mut entry = guard.clone().unwrap_or(Identity { vendor: String::new(), product: String::new() });
                    if let Some(v) = vendor {
                        entry.vendor = v;
                    }
                    if let Some(p) = product {
                        entry.product = p;
                    }
                    *guard = Some(entry);
                    identity_updated = true;
                    continue;
                }

                let mut guard = state.lock().await;
                let channel = match &update {
                    Update::Mute { channel, .. }
                    | Update::FrequencyMhz { channel, .. }
                    | Update::Rssi { channel, .. }
                    | Update::Rsqi { channel, .. }
                    | Update::Divi { channel, .. }
                    | Update::Af { channel, .. }
                    | Update::BatteryGauge { channel, .. }
                    | Update::BatteryLifetime { channel, .. } => *channel,
                    Update::Identity { .. } => unreachable!(),
                };
                let entry = guard.entry(channel).or_insert_with(empty_state);
                match update {
                    Update::Mute { muted, .. } => entry.muted = muted,
                    Update::FrequencyMhz { mhz, .. } => entry.frequency_mhz = Some(mhz),
                    Update::Rssi { dbm, .. } => entry.rf_level_dbm = Some(dbm),
                    Update::Rsqi { percent, .. } => entry.rf_quality_percent = Some(percent),
                    Update::Divi { antenna, .. } => entry.antenna = antenna,
                    Update::Af { dbfs, .. } => entry.audio_level_dbfs = Some(dbfs),
                    Update::BatteryGauge { percent, .. } => entry.battery_percent = percent,
                    Update::BatteryLifetime { minutes, .. } => entry.battery_minutes_remaining = minutes,
                    Update::Identity { .. } => unreachable!(),
                }
                drop(guard);
                if !touched_channels.contains(&channel) {
                    touched_channels.push(channel);
                }
            }

            if identity_updated {
                debug!(device = %id, "Sennheiser device identity update");
            }

            for channel in touched_channels {
                let new_state = *state.lock().await.get(&channel).unwrap();
                debug!(device = %id, channel, "Sennheiser telemetry update");
                let _ = tx.send(MicEvent {
                    address: MicAddress::new(id.to_string(), channel),
                    state: new_state,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_mute_and_frequency_converting_khz_to_mhz() {
        let root = json!({"rx1": {"mute": true, "frequency": 614125}});
        let updates = extract_updates(&root);
        assert!(updates.contains(&Update::Mute { channel: 1, muted: true }));
        assert!(updates.contains(&Update::FrequencyMhz { channel: 1, mhz: 614.125 }));
    }

    #[test]
    fn extracts_metering_fields_with_documented_antenna_codes() {
        let root = json!({"m": {"rx2": {"rssi": -45.5, "rsqi": 92, "divi": 2, "af": -20.3}}});
        let updates = extract_updates(&root);
        assert!(updates.contains(&Update::Rssi { channel: 2, dbm: -45.5 }));
        assert!(updates.contains(&Update::Rsqi { channel: 2, percent: 92 }));
        assert!(updates.contains(&Update::Divi { channel: 2, antenna: Some(AntennaDiversity::B) }));
        assert!(updates.contains(&Update::Af { channel: 2, dbfs: -20.3 }));
    }

    #[test]
    fn extracts_battery_using_matching_rx_channel_as_tx_index() {
        let root = json!({"mates": {"tx3": {"battery": {"gauge": 65, "lifetime": 312}}}});
        let updates = extract_updates(&root);
        assert!(updates.contains(&Update::BatteryGauge { channel: 3, percent: Some(65) }));
        assert!(updates.contains(&Update::BatteryLifetime { channel: 3, minutes: Some(312) }));
    }

    #[test]
    fn missing_battery_fields_stay_none_not_guessed() {
        // e.g. what the doc's error-424 "TX not present" case would look
        // like if the field is simply absent rather than a number.
        let root = json!({"mates": {"tx1": {"battery": {}}}});
        let updates = extract_updates(&root);
        assert!(updates.contains(&Update::BatteryGauge { channel: 1, percent: None }));
        assert!(updates.contains(&Update::BatteryLifetime { channel: 1, minutes: None }));
    }

    #[test]
    fn extracts_device_identity() {
        let root = json!({"device": {"identity": {"vendor": "Sennheiser electronic SE & CO KG", "product": "EW-DX EM 2"}}});
        let updates = extract_updates(&root);
        assert_eq!(
            updates,
            vec![Update::Identity {
                vendor: Some("Sennheiser electronic SE & CO KG".to_string()),
                product: Some("EW-DX EM 2".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn get_state_subscribes_once_and_the_initial_notification_populates_it() {
        use tokio::net::UdpSocket as TestSocket;

        let server = TestSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let mut adapter = SennheiserAdapter::new("ewdx-1", server_addr);
        let mut events = adapter.subscribe();
        adapter.connect().await.unwrap();

        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, client_addr) = server.recv_from(&mut buf).await.unwrap();
            let received: Value = serde_json::from_slice(&buf[..len]).unwrap();

            // Confirm the lazy subscribe call actually asked for channel
            // 1's mute/frequency/metering/battery leaves, structurally
            // (not string-matching, since JSON key order isn't guaranteed).
            let sub = &received["osc"]["state"]["subscribe"][0];
            assert!(sub["rx1"]["mute"].is_null());
            assert!(sub["m"]["rx1"]["rssi"].is_null());
            assert!(sub["mates"]["tx1"]["battery"]["gauge"].is_null());
            assert_eq!(sub["#"]["lifetime"], SUBSCRIBE_LIFETIME_SECS);

            // Reply as the initial subscription notification would: the
            // resolved current values for the subscribed leaves.
            let reply = json!({
                "rx1": {"mute": false},
                "m": {"rx1": {"rssi": -50.0, "rsqi": 88, "divi": 1, "af": -18.0}},
                "mates": {"tx1": {"battery": {"gauge": 72, "lifetime": 245}}}
            });
            server.send_to(reply.to_string().as_bytes(), client_addr).await.unwrap();
        });

        let state = tokio::time::timeout(Duration::from_secs(2), adapter.get_state(1))
            .await
            .expect("timed out waiting for get_state to resolve")
            .unwrap();

        responder.await.unwrap();

        assert_eq!(state.battery_percent, Some(72));
        assert_eq!(state.battery_minutes_remaining, Some(245));
        assert_eq!(state.rf_level_dbm, Some(-50.0));
        assert_eq!(state.rf_quality_percent, Some(88));
        assert_eq!(state.antenna, Some(AntennaDiversity::A));
        assert_eq!(state.audio_level_dbfs, Some(-18.0));
        assert!(!state.muted);

        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timed out waiting for broadcast event")
            .unwrap();
        assert_eq!(event.address, MicAddress::new("ewdx-1", 1));
    }
}

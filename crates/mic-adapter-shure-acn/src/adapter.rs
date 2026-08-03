//! `MicAdapter` over the ACN control path. See the crate doc comment for
//! what this can and cannot observe.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use dante_babelbox_core::{AdapterError, AdapterResult, DeviceInfo};
use dante_babelbox_mic_core::{MicAdapter, MicAddress, MicEvent, MicState};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::acn::{
    properties_in_datagram, PropertyReport, Value, NO_RECENT_DATA, PROP_BATTERY_BARS,
    PROP_CHANNEL_NAME, PROP_DEVICE_NAME, PROP_FREQUENCY_KHZ, PROP_MODEL_NAME, PROP_RF_BARS,
    PROP_RF_LEVEL_DBM, RF_FLOOR_DBM, SDT_PORT,
};
use crate::slp::{parse_attr_reply, Advertisement, SLP_GROUP, SLP_PORT};

/// A QLX-D receiver is a single-channel device; the channel argument is
/// accepted for trait compatibility and must be 1.
const ONLY_CHANNEL: u16 = 1;

/// Everything observed about the receiver so far.
#[derive(Debug, Clone, Default)]
struct Observed {
    advertisement: Option<Advertisement>,
    device_name: Option<String>,
    channel_name: Option<String>,
    frequency_mhz: Option<f64>,
    rf_level_dbm: Option<i32>,
    rf_bars: Option<u8>,
    battery_bars: Option<u8>,
    /// Set when a carrier has just been acquired, so the next battery
    /// reading - which is a known transient - can be discarded.
    battery_reading_suspect: bool,
}

impl Observed {
    /// Applies one property report. Returns true if telemetry changed in a
    /// way worth publishing.
    fn apply(&mut self, report: &PropertyReport) -> bool {
        match (report.address, &report.value) {
            (PROP_MODEL_NAME, Value::Text(t)) => {
                self.advertisement
                    .get_or_insert_with(Advertisement::default)
                    .model = Some(t.clone());
                false
            }
            (PROP_DEVICE_NAME, Value::Text(t)) => {
                self.device_name = Some(t.clone());
                false
            }
            (PROP_CHANNEL_NAME, Value::Text(t)) => {
                self.channel_name = Some(t.clone());
                false
            }
            (PROP_FREQUENCY_KHZ, Value::UInt32(khz)) => {
                self.frequency_mhz = Some(*khz as f64 / 1000.0);
                true
            }
            (PROP_RF_LEVEL_DBM, Value::Int32(dbm)) => {
                self.rf_level_dbm = Some(*dbm);
                true
            }
            (PROP_RF_BARS, Value::UInt8(bars)) => {
                let had_carrier = self.rf_bars.is_some_and(|b| b > 0);
                // Acquiring a carrier makes the receiver emit one battery
                // reading that does not match its own front panel, then
                // settle within ~4 s. Discarding it is what stops a run of
                // reacquisitions looking like a battery draining.
                if !had_carrier && *bars > 0 {
                    self.battery_reading_suspect = true;
                }
                self.rf_bars = Some(*bars);
                true
            }
            (PROP_BATTERY_BARS, Value::Int8(bars)) => {
                if self.battery_reading_suspect {
                    self.battery_reading_suspect = false;
                    debug!("discarding the battery reading that follows carrier acquisition");
                    return false;
                }
                // -1 means "no recent data", not "no carrier" - when a
                // carrier drops the value holds until a longer dropout.
                self.battery_bars = (*bars != NO_RECENT_DATA).then_some(*bars as u8);
                true
            }
            _ => false,
        }
    }

    fn to_mic_state(&self) -> MicState {
        MicState {
            // The wire carries bars, never a percentage.
            battery_percent: None,
            battery_bars: self.battery_bars,
            // Subscribed but never observed to emit; see the crate docs.
            battery_minutes_remaining: None,
            // With no carrier the receiver reports a flat floor rather
            // than a measurement, so that is reported as "no reading"
            // instead of as a very weak signal.
            rf_level_dbm: match (self.rf_level_dbm, self.rf_bars) {
                (Some(dbm), Some(0)) if dbm <= RF_FLOOR_DBM => None,
                (Some(dbm), _) => Some(dbm as f32),
                (None, _) => None,
            },
            // Bars are a 0-5 segment count, not a percentage.
            rf_quality_percent: None,
            // 0x02000812 is not audio - see the crate docs.
            audio_level_dbfs: None,
            // No mute property was identified on this path.
            muted: false,
            frequency_mhz: self.frequency_mhz,
            antenna: None,
        }
    }
}

pub struct ShureAcnAdapter {
    id: Arc<str>,
    /// Local interface to join multicast groups on.
    interface: Ipv4Addr,
    /// Receiver address, once known.
    receiver: Option<IpAddr>,
    tx: broadcast::Sender<MicEvent>,
    observed: Arc<Mutex<Observed>>,
    cancel: CancellationToken,
    connected: bool,
}

impl ShureAcnAdapter {
    pub fn new(id: impl Into<Arc<str>>, interface: Ipv4Addr) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            id: id.into(),
            interface,
            receiver: None,
            tx,
            observed: Arc::new(Mutex::new(Observed::default())),
            cancel: CancellationToken::new(),
            connected: false,
        }
    }

    fn check_channel(channel: u16) -> AdapterResult<()> {
        (channel == ONLY_CHANNEL)
            .then_some(())
            .ok_or(AdapterError::UnsupportedChannel(channel))
    }
}

#[async_trait]
impl MicAdapter for ShureAcnAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn connect(&mut self) -> AdapterResult<()> {
        // Discovery: passive, works from any host on the segment.
        let slp = bind_multicast(SLP_GROUP, SLP_PORT, self.interface)?;
        spawn_discovery_loop(
            Arc::new(slp),
            Arc::clone(&self.id),
            Arc::clone(&self.observed),
            self.cancel.clone(),
        );

        // Telemetry: only visible with a mirrored port or on the console
        // itself, because the receiver unicasts its events to the console.
        // Binding is still worth doing - it costs nothing and works in
        // exactly the setups where it can work.
        match bind_multicast([0, 0, 0, 0], SDT_PORT, self.interface) {
            Ok(sdt) => spawn_telemetry_loop(
                Arc::new(sdt),
                Arc::clone(&self.id),
                self.tx.clone(),
                Arc::clone(&self.observed),
                self.cancel.clone(),
            ),
            Err(e) => warn!(
                device = %self.id,
                error = %e,
                "could not bind the SDT port; discovery will still work but no telemetry will be seen"
            ),
        }

        self.connected = true;
        Ok(())
    }

    async fn identify(&mut self) -> AdapterResult<DeviceInfo> {
        if !self.connected {
            return Err(AdapterError::Connection("not connected".into()));
        }
        let observed = self.observed.lock().await;
        let model = observed
            .advertisement
            .as_ref()
            .and_then(|a| a.model.clone())
            .ok_or_else(|| {
                AdapterError::Protocol(
                    "no SLP advertisement heard yet; receivers advertise every ~2 s, so either \
                     none is present or this host is not on their segment"
                        .into(),
                )
            })?;
        Ok(DeviceInfo {
            vendor: "Shure".to_string(),
            model,
            address: self.receiver.unwrap_or(IpAddr::V4(self.interface)),
        })
    }

    async fn get_state(&mut self, channel: u16) -> AdapterResult<MicState> {
        Self::check_channel(channel)?;
        Ok(self.observed.lock().await.to_mic_state())
    }

    /// Not supported: no mute property was identified anywhere in the
    /// property map, and the only writable property observed was AF output
    /// level. Erroring is deliberate - writing to a guessed address could
    /// change something else on a live receiver.
    async fn set_mute(&mut self, _channel: u16, _muted: bool) -> AdapterResult<()> {
        Err(AdapterError::Protocol(
            "ACN exposes no identified mute property on QLX-D; use the Command Strings adapter \
             (mic-adapter-shure) for mute control"
                .into(),
        ))
    }

    fn subscribe(&self) -> broadcast::Receiver<MicEvent> {
        self.tx.subscribe()
    }
}

/// Binds a UDP port with address reuse, joining `group` when it is a real
/// multicast address.
fn bind_multicast(group: [u8; 4], port: u16, interface: Ipv4Addr) -> AdapterResult<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .set_reuse_address(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    #[cfg(unix)]
    socket
        .set_reuse_port(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| AdapterError::Connection(e.to_string()))?;
    socket
        .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())
        .map_err(|e| AdapterError::Connection(format!("binding UDP {port}: {e}")))?;

    let group = Ipv4Addr::from(group);
    if group.is_multicast() {
        socket
            .join_multicast_v4(&group, &interface)
            .map_err(|e| AdapterError::Connection(format!("joining {group}: {e}")))?;
    }
    UdpSocket::from_std(socket.into()).map_err(|e| AdapterError::Connection(e.to_string()))
}

fn spawn_discovery_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    observed: Arc<Mutex<Observed>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let len = tokio::select! {
                _ = cancel.cancelled() => return,
                result = socket.recv(&mut buf) => match result {
                    Ok(len) => len,
                    Err(e) => {
                        warn!(device = %id, error = %e, "SLP socket read failed");
                        return;
                    }
                },
            };
            if let Some(ad) = parse_attr_reply(&buf[..len]) {
                if ad.is_receiver() {
                    debug!(device = %id, model = ?ad.model, name = ?ad.user_name, "discovered receiver");
                    observed.lock().await.advertisement = Some(ad);
                }
            }
        }
    });
}

fn spawn_telemetry_loop(
    socket: Arc<UdpSocket>,
    id: Arc<str>,
    tx: broadcast::Sender<MicEvent>,
    observed: Arc<Mutex<Observed>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let len = tokio::select! {
                _ = cancel.cancelled() => return,
                result = socket.recv(&mut buf) => match result {
                    Ok(len) => len,
                    Err(e) => {
                        warn!(device = %id, error = %e, "SDT socket read failed");
                        return;
                    }
                },
            };
            let reports = properties_in_datagram(&buf[..len]);
            if reports.is_empty() {
                continue;
            }
            let state = {
                let mut guard = observed.lock().await;
                // Every report is applied; `any`-style short-circuiting
                // would drop the rest of the datagram.
                let mut changed = false;
                for report in &reports {
                    changed |= guard.apply(report);
                }
                if !changed {
                    continue;
                }
                guard.to_mic_state()
            };
            let _ = tx.send(MicEvent {
                address: MicAddress::new(id.to_string(), ONLY_CHANNEL),
                state,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acn::PROP_UNRESOLVED_LEVEL;

    fn report(address: u32, value: Value) -> PropertyReport {
        PropertyReport { address, value }
    }

    #[test]
    fn battery_bars_are_reported_as_bars_and_never_as_a_percentage() {
        let mut o = Observed::default();
        assert!(o.apply(&report(PROP_BATTERY_BARS, Value::Int8(2))));

        let state = o.to_mic_state();
        assert_eq!(state.battery_bars, Some(2));
        assert_eq!(
            state.battery_percent, None,
            "bars must not be multiplied out into a percentage"
        );
    }

    #[test]
    fn a_minus_one_battery_reading_means_no_data_not_zero_bars() {
        let mut o = Observed::default();
        o.apply(&report(PROP_BATTERY_BARS, Value::Int8(4)));
        o.apply(&report(PROP_BATTERY_BARS, Value::Int8(NO_RECENT_DATA)));
        assert_eq!(
            o.to_mic_state().battery_bars,
            None,
            "-1 is 'no recent data', which is not the same as a flat battery"
        );
    }

    #[test]
    fn the_battery_reading_after_carrier_acquisition_is_discarded() {
        let mut o = Observed::default();
        o.apply(&report(PROP_RF_BARS, Value::UInt8(0))); // no carrier
        o.apply(&report(PROP_RF_BARS, Value::UInt8(3))); // carrier acquired

        // The receiver's first post-acquisition reading is a known
        // transient that disagrees with its own front panel.
        assert!(!o.apply(&report(PROP_BATTERY_BARS, Value::Int8(3))));
        assert_eq!(o.to_mic_state().battery_bars, None);

        // The next one settles and is trusted.
        assert!(o.apply(&report(PROP_BATTERY_BARS, Value::Int8(2))));
        assert_eq!(o.to_mic_state().battery_bars, Some(2));
    }

    #[test]
    fn the_rf_floor_with_no_carrier_is_reported_as_no_reading() {
        let mut o = Observed::default();
        o.apply(&report(PROP_RF_LEVEL_DBM, Value::Int32(RF_FLOOR_DBM)));
        o.apply(&report(PROP_RF_BARS, Value::UInt8(0)));
        assert_eq!(
            o.to_mic_state().rf_level_dbm,
            None,
            "-50 dBm with 0 bars is a floor, not a measurement"
        );

        o.apply(&report(PROP_RF_LEVEL_DBM, Value::Int32(-27)));
        o.apply(&report(PROP_RF_BARS, Value::UInt8(4)));
        assert_eq!(o.to_mic_state().rf_level_dbm, Some(-27.0));
    }

    #[test]
    fn the_unresolved_level_never_reaches_audio_level_dbfs() {
        let mut o = Observed::default();
        o.apply(&report(PROP_UNRESOLVED_LEVEL, Value::Int16(-55)));
        assert_eq!(
            o.to_mic_state().audio_level_dbfs,
            None,
            "0x02000812 is not audio and must never be presented as it"
        );
    }

    #[test]
    fn frequency_is_converted_from_khz_to_mhz() {
        let mut o = Observed::default();
        o.apply(&report(PROP_FREQUENCY_KHZ, Value::UInt32(606_700)));
        assert_eq!(o.to_mic_state().frequency_mhz, Some(606.700));
    }

    #[tokio::test]
    async fn set_mute_refuses_rather_than_writing_to_a_guessed_address() {
        let mut a = ShureAcnAdapter::new("qlxd-1", Ipv4Addr::LOCALHOST);
        let err = a.set_mute(1, true).await.unwrap_err();
        let AdapterError::Protocol(message) = err else {
            panic!("expected a protocol error");
        };
        assert!(
            message.contains("mic-adapter-shure"),
            "should point at the working path"
        );
    }

    #[tokio::test]
    async fn only_channel_one_exists_on_a_single_channel_receiver() {
        let mut a = ShureAcnAdapter::new("qlxd-1", Ipv4Addr::LOCALHOST);
        assert!(a.get_state(1).await.is_ok());
        assert!(matches!(
            a.get_state(2).await,
            Err(AdapterError::UnsupportedChannel(2))
        ));
    }

    #[tokio::test]
    async fn identify_explains_itself_when_nothing_has_been_heard() {
        let mut a = ShureAcnAdapter::new("qlxd-1", Ipv4Addr::LOCALHOST);
        a.connected = true;
        let err = a.identify().await.unwrap_err();
        let AdapterError::Protocol(message) = err else {
            panic!("expected a protocol error");
        };
        assert!(message.contains("every ~2 s"));
    }
}

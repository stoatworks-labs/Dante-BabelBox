use std::net::SocketAddr;
use std::time::Duration;

use dante_babelbox_mic_adapter_sennheiser::SennheiserAdapter;
use dante_babelbox_mic_adapter_shure::ShureAdapter;
use dante_babelbox_mic_core::{MicAdapter, MicEvent};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use crate::config::{Config, MicKind};

const DEFAULT_SHURE_PORT: u16 = 2202;
const DEFAULT_SENNHEISER_PORT: u16 = 45;

/// Channels proactively primed after connecting. Shure's `connect()`
/// already turns on metering for every channel in one broadcast command
/// (channel `0`), so priming is a harmless no-op there; Sennheiser has no
/// such broadcast, so each channel needs its own `get_state` call to kick
/// off a subscription (see `SennheiserAdapter`'s module doc comment).
/// 1-4 covers every receiver model in scope here (largest is 4-channel);
/// priming a channel a smaller unit doesn't have just times out and is
/// logged, not fatal.
const CHANNELS_TO_PRIME: std::ops::RangeInclusive<u16> = 1..=4;

/// Connects every configured mic, prints live telemetry as it arrives,
/// and runs until interrupted. Pure monitoring - no mapping/routing
/// between devices (see mic-core's module doc comment for why radio-mic
/// telemetry doesn't need the preamp bridge's `Router`).
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let mut receivers = Vec::new();
    // Adapters must stay alive for the program's duration - dropping one
    // closes its socket. Only `subscribe()`'s Receiver is used below, but
    // the adapter itself has to be kept somewhere for that to keep working.
    let mut adapters: Vec<Box<dyn MicAdapter>> = Vec::new();

    for mic in &cfg.mics {
        let mut adapter: Box<dyn MicAdapter> = match mic.kind {
            MicKind::ShureUlxd | MicKind::ShureAxient => {
                let addr = SocketAddr::new(mic.address, mic.port.unwrap_or(DEFAULT_SHURE_PORT));
                Box::new(ShureAdapter::new(mic.id.clone(), addr))
            }
            MicKind::SennheiserEwdx => {
                let addr = SocketAddr::new(mic.address, mic.port.unwrap_or(DEFAULT_SENNHEISER_PORT));
                Box::new(SennheiserAdapter::new(mic.id.clone(), addr))
            }
        };

        adapter
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("connecting to mic '{}' at {}: {e}", mic.id, mic.address))?;
        info!(mic = %mic.id, address = %mic.address, "connected");

        for channel in CHANNELS_TO_PRIME {
            match tokio::time::timeout(Duration::from_millis(1200), adapter.get_state(channel)).await {
                Ok(Ok(_)) => debug!(mic = %mic.id, channel, "channel present"),
                Ok(Err(e)) => debug!(mic = %mic.id, channel, error = %e, "channel not available"),
                Err(_) => debug!(mic = %mic.id, channel, "timed out priming channel"),
            }
        }

        receivers.push(adapter.subscribe());
        adapters.push(adapter);
    }

    info!("watching {} mic(s) - press Ctrl-C to stop", receivers.len());

    let mut tasks = Vec::new();
    for mut rx in receivers {
        tasks.push(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => println!("{}", format_event(&event)),
                    Err(RecvError::Lagged(n)) => warn!("dropped {n} telemetry update(s), receiver lagged"),
                    Err(RecvError::Closed) => return,
                }
            }
        }));
    }

    tokio::select! {
        _ = join_all_tasks(tasks) => {}
        result = tokio::signal::ctrl_c() => {
            result?;
            info!("shutting down");
        }
    }

    Ok(())
}

async fn join_all_tasks(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

fn fmt_opt<T: std::fmt::Display>(value: Option<T>, unit: &str) -> String {
    match value {
        Some(v) => format!("{v}{unit}"),
        None => "n/a".to_string(),
    }
}

fn format_event(event: &MicEvent) -> String {
    let s = &event.state;
    let antenna = match s.antenna {
        Some(dante_babelbox_mic_core::AntennaDiversity::A) => "A",
        Some(dante_babelbox_mic_core::AntennaDiversity::B) => "B",
        Some(dante_babelbox_mic_core::AntennaDiversity::Inactive) => "none",
        None => "n/a",
    };
    format!(
        "{} ch{}: battery={} runtime={} rf={} quality={} af={} antenna={} freq={} mute={}",
        event.address.device_id,
        event.address.channel,
        fmt_opt(s.battery_percent, "%"),
        fmt_opt(s.battery_minutes_remaining, "min"),
        fmt_opt(s.rf_level_dbm, "dBm"),
        fmt_opt(s.rf_quality_percent, "%"),
        fmt_opt(s.audio_level_dbfs, "dBFS"),
        antenna,
        fmt_opt(s.frequency_mhz, "MHz"),
        s.muted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dante_babelbox_mic_core::{AntennaDiversity, MicAddress, MicState};

    #[test]
    fn formats_event_with_missing_fields_as_na() {
        let event = MicEvent {
            address: MicAddress::new("ulxd-1", 1),
            state: MicState {
                battery_percent: Some(82),
                battery_minutes_remaining: None,
                rf_level_dbm: Some(-45.0),
                rf_quality_percent: None,
                audio_level_dbfs: None,
                muted: false,
                frequency_mhz: Some(614.125),
                antenna: Some(AntennaDiversity::A),
            },
        };
        let line = format_event(&event);
        assert!(line.starts_with("ulxd-1 ch1:"));
        assert!(line.contains("battery=82%"));
        assert!(line.contains("runtime=n/a"));
        assert!(line.contains("rf=-45dBm"));
        assert!(line.contains("antenna=A"));
        assert!(line.contains("mute=false"));
    }
}

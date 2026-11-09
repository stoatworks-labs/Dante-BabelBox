//! Runs an emulated Rio on its own, with everything it hears printed: the
//! bench tool for the first time a real console is put in front of it.
//!
//! ```text
//! cargo run -p dante-babelbox-preamp-adapter-yamaha --example emulated_rio -- \
//!     <this machine's address on the Dante network> <unit id> [--no-mdns] [--mbc-only] [--set CH DB]
//! ```
//!
//! `preamp-bridge` can run the same device (kind `yamaha-rio-emulated`),
//! but a plugin's log lines do not reach the bridge's log. Here they do, and
//! the lines to watch for are "... with no known reply": a question the
//! console asked that no capture has an answer for. Set
//! `RUST_LOG=debug` for more.
//!
//! - `--no-mdns` answers Dante queries but does not advertise.
//! - `--mbc-only` runs the head-amp protocol alone: no ARC, no ConMon
//!   status, no mDNS. Use it to find out which layers a console needs.
//! - `--set CH DB` changes a gain from this end five seconds after start,
//!   and again every ten, to see whether the console follows.
//!
//! Capture the session while it runs; that capture is worth more than the
//! result:
//!
//! ```text
//! sudo tcpdump -i <interface> -w emulated-rio-bench.pcap 'not (udp portrange 14336-14591)'
//! ```

use std::env;
use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::time::Duration;

use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_preamp_adapter_yamaha::{EmulatedRioAdapter, EmulatedRioConfig};

fn parse_unit_id(text: &str) -> Option<u16> {
    let value = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) => u16::from_str_radix(digits, 16).ok()?,
        None => text.parse().ok()?,
    };
    (1..=0xfff).contains(&value).then_some(value)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = env::args().skip(1).collect();
    let positional: Vec<&String> = args.iter().take_while(|a| !a.starts_with("--")).collect();
    let (Some(interface), Some(unit_id)) = (
        positional.first().and_then(|a| a.parse::<Ipv4Addr>().ok()),
        positional.get(1).and_then(|a| parse_unit_id(a)),
    ) else {
        eprintln!("usage: emulated_rio <local address> <unit id> [--no-mdns] [--mbc-only] [--set CH DB]");
        return ExitCode::FAILURE;
    };

    let mut config = EmulatedRioConfig::new(interface, unit_id);
    config.advertise = !args.iter().any(|a| a == "--no-mdns");
    config.presence = !args.iter().any(|a| a == "--mbc-only");
    let nudge: Option<(u16, f32)> = args.iter().position(|a| a == "--set").and_then(|i| {
        Some((args.get(i + 1)?.parse().ok()?, args.get(i + 2)?.parse().ok()?))
    });

    let mut adapter = EmulatedRioAdapter::new("emulated-rio", config);
    let identity = adapter.identity();
    let mut events = adapter.subscribe();
    if let Err(e) = adapter.connect().await {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    println!("{} is on the network at {}", identity.name, identity.ip);
    println!("mount unit id {unit_id} on the console, then move a head-amp gain. Ctrl-C to stop.");

    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    ticker.reset_after(Duration::from_secs(5));
    let mut up = true;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = events.recv() => match event {
                Ok(event) => {
                    let what = match (event.changed.gain, event.changed.phantom) {
                        (true, _) => format!("gain {:+.0} dB", event.state.gain_db),
                        (_, true) => format!("+48V {}", if event.state.phantom { "on" } else { "off" }),
                        _ => "nothing".to_string(),
                    };
                    println!("console -> input {:>2}: {what}", event.address.channel);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    println!("({missed} changes not shown)");
                }
                Err(_) => break,
            },
            _ = ticker.tick(), if nudge.is_some() => {
                let (channel, db) = nudge.expect("checked by the guard");
                let value = if up { db } else { db - 3.0 };
                up = !up;
                match adapter.set_gain(channel, value).await {
                    Ok(()) => println!("here    -> input {channel:>2}: gain {value:+.0} dB (does the console show it?)"),
                    Err(e) => println!("could not set input {channel}: {e}"),
                }
            }
        }
    }
    let _ = adapter.disconnect().await;
    println!("{} has left the network", identity.name);
    ExitCode::SUCCESS
}

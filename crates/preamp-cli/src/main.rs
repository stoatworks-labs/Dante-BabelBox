use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod config;
mod daemon;

#[derive(Parser)]
#[command(name = "preamp-bridge", version, about = "Cross-vendor Dante preamp control bridge")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Browse Dante's mDNS advertisements for devices on the LAN.
    Discover {
        #[arg(long, default_value_t = 5)]
        timeout_secs: u64,
    },
    /// Run the bridge daemon using a bridge.toml config.
    Run {
        #[arg(long, default_value = "bridge.toml")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Discover { timeout_secs } => {
            let devices = dante_babelbox_discovery::discover(Duration::from_secs(timeout_secs)).await?;
            if devices.is_empty() {
                println!("No Dante devices found.");
            } else {
                for d in devices {
                    println!("{}  {:?}:{}", d.name, d.addresses, d.port);
                }
            }
        }
        Command::Run { config: config_path } => {
            let cfg = config::Config::load(&config_path)?;
            println!(
                "Loaded {} device(s) and {} mapping(s) from {}",
                cfg.devices.len(),
                cfg.mappings.len(),
                config_path.display()
            );
            for d in &cfg.devices {
                println!("  device   {:<16} {:?} @ {}", d.id, d.kind, d.address);
            }
            for m in &cfg.mappings {
                println!(
                    "  mapping  {}:{} {} {}:{}",
                    m.from.device_id,
                    m.from.channel,
                    if m.bidirectional { "<->" } else { "-->" },
                    m.to.device_id,
                    m.to.channel
                );
            }
            daemon::run(cfg, Some(config_path)).await?;
        }
    }

    Ok(())
}

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod config;
mod watch;

use config::Config;

#[derive(Parser)]
#[command(name = "mic-monitor", version, about = "Cross-vendor radio-mic telemetry monitor")]
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
    /// Connect to the mics in mics.toml and print live telemetry.
    Watch {
        #[arg(long, default_value = "mics.toml")]
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
        Command::Watch { config: config_path } => {
            let cfg = Config::load(&config_path)?;
            println!("Loaded {} mic(s) from {}", cfg.mics.len(), config_path.display());
            watch::run(cfg).await?;
        }
    }

    Ok(())
}

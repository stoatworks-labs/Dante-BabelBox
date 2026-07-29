use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod config;
mod watch;

use config::Config;

#[derive(Parser, serde::Serialize)]
#[command(name = "mic-monitor", version, about = "Cross-vendor radio-mic telemetry monitor")]
struct Cli {
    // Skipped rather than made Serialize: which subcommand ran is already
    // in `process.args` in every report, and deriving Serialize across the
    // whole command enum would drag the derive through every variant.
    #[serde(skip)]
    #[command(subcommand)]
    command: Command,

    /// Write a diagnostics bundle and exit.
    #[arg(long)]
    collect_diagnostics: bool,
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
    let cli = Cli::parse();

    // Before anything that can fail, so a failure during startup is logged
    // and lands in a crash report like any other.
    let _diag = diag::init(
        diag::Options::new("mic-monitor", "BABELBOX", env!("CARGO_PKG_VERSION"))
            .with_config(&cli),
    )?;

    if cli.collect_diagnostics {
        println!("{}", diag::collect_diagnostics()?.display());
        return Ok(());
    }

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

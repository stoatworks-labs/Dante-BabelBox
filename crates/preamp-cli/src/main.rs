use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod config;
mod daemon;
mod init;
mod ports;

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
        /// Address to serve the patch-bay web UI on. Defaults to all
        /// interfaces so anyone on the LAN can reach it - same trust
        /// model as a hardware router's control port (no auth, no TLS;
        /// meant for a trusted operations network). Use 127.0.0.1:PORT
        /// to restrict it to this machine only.
        #[arg(long, default_value = "0.0.0.0:8080")]
        web_bind: SocketAddr,
        /// Don't serve the patch-bay web UI at all.
        #[arg(long)]
        no_web: bool,
        /// Directory to scan for dynamically-loadable device plugin
        /// `.so`/`.dylib`/`.dll` files. Every real vendor adapter ships
        /// this way; only a couple of explanatory-error placeholder
        /// kinds are built in.
        #[arg(long, default_value = "plugins")]
        plugins_dir: PathBuf,
    },
    /// Discover devices and auto-generate the [[device]] blocks of a
    /// bridge.toml. [[mapping]] entries must still be added by hand,
    /// unless --infer-mappings is passed.
    Init {
        #[arg(long, default_value = "bridge.toml")]
        output: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_secs: u64,
        /// Overwrite an existing output file.
        #[arg(long)]
        force: bool,
        /// Also guess [[mapping]] entries by observing live Dante audio
        /// routing. A real signal, not a guess, but the resulting channel
        /// numbers are Dante audio channel numbers, which only
        /// conventionally (not necessarily) match a device's preamp
        /// channel numbers - verify before trusting. See the written
        /// file's header comment when this is set.
        #[arg(long)]
        infer_mappings: bool,
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
        Command::Run {
            config: config_path,
            web_bind,
            no_web,
            plugins_dir,
        } => {
            let cfg = config::Config::load(&config_path)?;
            println!(
                "Loaded {} device(s) and {} mapping(s) from {}",
                cfg.devices.len(),
                cfg.mappings.len(),
                config_path.display()
            );
            for d in &cfg.devices {
                let where_ = match d.address {
                    Some(addr) => addr.to_string(),
                    None => "virtual".to_string(),
                };
                println!("  device   {:<16} {:?} @ {}", d.id, d.kind, where_);
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
            daemon::run(cfg, Some(config_path), if no_web { None } else { Some(web_bind) }, plugins_dir).await?;
        }
        Command::Init {
            output,
            timeout_secs,
            force,
            infer_mappings,
        } => {
            init::run(output, Duration::from_secs(timeout_secs), force, infer_mappings).await?;
        }
    }

    Ok(())
}

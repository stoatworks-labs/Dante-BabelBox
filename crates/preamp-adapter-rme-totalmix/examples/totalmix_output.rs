//! Bench check: read one TotalMix output fader, and optionally move it.
//!
//! ```text
//! cargo run -p dante-babelbox-preamp-adapter-rme-totalmix --example totalmix_output -- <output> [dB]
//! ```
//!
//! Talks to TotalMix on 127.0.0.1:7001 and listens on 9001 (TotalMix's
//! defaults; set `TOTALMIX` to `host:port` to point elsewhere). With a level
//! it moves the fader there, then reads it back. Without one it only reads,
//! then reports every move of that fader made in TotalMix until Ctrl-C.

use std::net::SocketAddr;

use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_preamp_adapter_rme_totalmix::TotalMixAdapter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: totalmix_output <output, from 1> [dB]";
    let output: u16 = args.next().ok_or(usage)?.parse()?;
    let level: Option<f32> = args.next().map(|s| s.parse()).transpose()?;
    let totalmix: SocketAddr = std::env::var("TOTALMIX")
        .unwrap_or_else(|_| "127.0.0.1:7001".into())
        .parse()?;

    let mut adapter = TotalMixAdapter::new("totalmix", totalmix, output.max(1));
    adapter.connect().await?;
    let info = adapter.identify().await?;
    println!("{} {} at {}", info.vendor, info.model, info.address);

    let before = adapter.get_state(output).await?;
    println!("output {output}: {:.1} dB", before.gain_db);

    if let Some(db) = level {
        adapter.set_gain(output, db).await?;
        adapter.disconnect().await?;
        // Read it back on a fresh connection, so the answer is TotalMix's and
        // not the adapter's cache. The pause lets the old receive loop let go
        // of UDP 9001 first.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        adapter.connect().await?;
        let after = adapter.get_state(output).await?;
        println!(
            "output {output}: set {db:.1} dB, TotalMix reads {:.1} dB",
            after.gain_db
        );
        return Ok(());
    }

    let mut events = adapter.subscribe();
    println!("watching output {output}; move it in TotalMix (Ctrl-C to stop)");
    while let Ok(event) = events.recv().await {
        if event.address.channel == output {
            println!("output {output}: {:.1} dB", event.state.gain_db);
        }
    }
    Ok(())
}

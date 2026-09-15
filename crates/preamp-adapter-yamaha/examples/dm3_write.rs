//! WRITE test of `Dm3Adapter` against a real DM3, on ONE operator-nominated
//! channel only. Usage: dm3_write <ip> <channel> — channel is 1-based, and the
//! caller is responsible for it being a safe input (see the bench notes: ch 9+
//! with the stereo out muted). Sets gain to 12 dB then back to the value it
//! read first; toggles 48 V on then off. A second Dm3Adapter subscribes and
//! prints the events the console pushes back, proving the round trip.
use std::time::Duration;

use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_preamp_adapter_yamaha::Dm3Adapter;

#[tokio::main]
async fn main() {
    let ip = std::env::args().nth(1).expect("ip");
    let ch: u16 = std::env::args().nth(2).expect("channel").parse().expect("channel int");
    let addr = format!("{ip}:49900");

    let mut watcher = Dm3Adapter::new("watch", addr.parse().unwrap());
    watcher.connect().await.unwrap();
    let mut rx = watcher.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await { println!("   [subscribe] {ev:?}"); }
    });

    let mut a = Dm3Adapter::new("dm3", addr.parse().unwrap());
    a.connect().await.unwrap();

    let before = a.get_state(ch).await.expect("read before");
    println!("before: ch{ch} {before:?}");

    println!("set_gain({ch}, 12.0)");
    a.set_gain(ch, 12.0).await.expect("set_gain");
    tokio::time::sleep(Duration::from_millis(400)).await;
    println!("read back: {:?}", a.get_state(ch).await);

    println!("set_gain({ch}, {})  [restore]", before.gain_db);
    a.set_gain(ch, before.gain_db).await.expect("restore gain");
    tokio::time::sleep(Duration::from_millis(300)).await;

    println!("set_phantom({ch}, true)");
    a.set_phantom(ch, true).await.expect("phantom on");
    tokio::time::sleep(Duration::from_millis(700)).await;
    println!("set_phantom({ch}, false)  [restore]");
    a.set_phantom(ch, false).await.expect("phantom off");
    tokio::time::sleep(Duration::from_millis(500)).await;

    a.disconnect().await.ok();
    watcher.disconnect().await.ok();
}

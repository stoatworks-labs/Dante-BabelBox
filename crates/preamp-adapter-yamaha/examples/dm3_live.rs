//! Drive `Dm3Adapter` against a real DM3, read-only: identify, then
//! get_state for a few channels, then sit on `subscribe()` for a while and
//! print whatever the console pushes. Usage: dm3_live <ip> [secs]
use std::time::Duration;

use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_preamp_adapter_yamaha::Dm3Adapter;

#[tokio::main]
async fn main() {
    tracing_subscriber_init();
    let ip = std::env::args().nth(1).expect("ip");
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let mut a = Dm3Adapter::new("dm3", format!("{ip}:49900").parse().unwrap());
    a.connect().await.expect("connect");
    let t = std::time::Instant::now();
    match a.identify().await {
        Ok(info) => println!("identify: OK vendor={} model={} address={} ({:?})", info.vendor, info.model, info.address, t.elapsed()),
        Err(e) => println!("identify: ERR {e} ({:?})", t.elapsed()),
    }
    for ch in [1u16, 2, 16, 17] {
        let t = std::time::Instant::now();
        match a.get_state(ch).await {
            Ok(s) => println!("get_state({ch}): OK {s:?} ({:?})", t.elapsed()),
            Err(e) => println!("get_state({ch}): ERR {e} ({:?})", t.elapsed()),
        }
    }
    let mut rx = a.subscribe();
    println!("listening for pushed events for {secs}s ...");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ev)) => println!("event: {ev:?}"),
            Ok(Err(e)) => { println!("subscribe closed: {e}"); break; }
            Err(_) => break,
        }
    }
    a.disconnect().await.ok();
}

fn tracing_subscriber_init() {}

//! Exercise `Dm3ScpAdapter` against a real DM3 over SCP (TCP 49280).
//! Usage: dm3_scp_live <ip> [write-channel]
//! Read-only unless a 1-based write-channel is given (then it nudges that
//! channel's gain/phantom and restores them). A subscriber runs throughout
//! and prints every PreampEvent - proving the NOTIFY-driven change feed the
//! OSC adapter can't provide.
use std::time::Duration;

use dante_babelbox_core::DeviceAdapter;
use dante_babelbox_preamp_adapter_yamaha::Dm3ScpAdapter;

#[tokio::main]
async fn main() {
    let ip = std::env::args().nth(1).expect("ip");
    let write_ch: Option<u16> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let mut a = Dm3ScpAdapter::new("dm3", format!("{ip}:49280").parse().unwrap());
    a.connect().await.expect("connect");

    let mut rx = a.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            println!("   [event] ch{} gain={} phantom={} changed(g={},p={})",
                ev.address.channel, ev.state.gain_db, ev.state.phantom, ev.changed.gain, ev.changed.phantom);
        }
    });

    let t = std::time::Instant::now();
    match a.identify().await {
        Ok(i) => println!("identify: OK vendor={} model={} addr={} ({:?})", i.vendor, i.model, i.address, t.elapsed()),
        Err(e) => println!("identify: ERR {e}"),
    }
    for ch in [1u16, 2, 9, 16] {
        match a.get_state(ch).await {
            Ok(s) => println!("get_state({ch}): OK gain={} phantom={} pad={:?}", s.gain_db, s.phantom, s.pad),
            Err(e) => println!("get_state({ch}): ERR {e}"),
        }
    }

    if let Some(ch) = write_ch {
        let before = a.get_state(ch).await.expect("read before");
        println!("-- writing ch{ch} (was gain={} phantom={}) --", before.gain_db, before.phantom);
        a.set_gain(ch, 20.0).await.expect("set_gain");
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = a.get_state(ch).await.expect("read after");
        println!("after set_gain(20): get_state reads gain={} (stale-cache bug would read {})",
            after.gain_db, before.gain_db);
        a.set_phantom(ch, true).await.expect("phantom on");
        tokio::time::sleep(Duration::from_millis(500)).await;
        // restore
        a.set_gain(ch, before.gain_db).await.expect("restore gain");
        a.set_phantom(ch, before.phantom).await.expect("restore phantom");
        tokio::time::sleep(Duration::from_millis(400)).await;
        println!("-- restored ch{ch} --");
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    a.disconnect().await.ok();
}

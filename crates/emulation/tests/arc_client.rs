//! The workspace's own ARC client, put to the virtual device over real
//! sockets. `dante_babelbox_discovery::dante_control` was written against a
//! third party's documentation of the protocol and the responder against a
//! capture of a real device, so their agreeing is evidence about both.

use std::net::{Ipv4Addr, SocketAddr};

use dante_babelbox_discovery::dante_control;
use dante_babelbox_emulation::{DeviceIdentity, Persona, Ports, VirtualDevice, VirtualDeviceConfig};

async fn device() -> Option<VirtualDevice> {
    let config = VirtualDeviceConfig {
        identity: DeviceIdentity {
            name: "Y005-Yamaha-Rio3224-D2-a1b2c3".into(),
            mac: [0x02, 0xbb, 0x5a, 0xa1, 0xb2, 0xc3],
            ip: Ipv4Addr::LOCALHOST,
        },
        persona: Persona::yamaha_rio3224_d2(),
        presence: true,
        advertise: false,
        ports: Ports { arc: 0, cmc: 0, control: 0, vendor: 48731 },
    };
    match VirtualDevice::start(config).await {
        Ok(device) => Some(device),
        Err(e) => {
            // A stock Linux loopback has no multicast; the device cannot
            // join its vendor group there.
            eprintln!("skipping: {e}");
            None
        }
    }
}

#[tokio::test]
async fn the_workspace_arc_client_reads_the_virtual_device() {
    let Some(device) = device().await else { return };
    let arc = SocketAddr::from((Ipv4Addr::LOCALHOST, device.ports().arc));

    let name = dante_control::query_device_name(arc).await.unwrap();
    assert_eq!(name.as_deref(), Some("Y005-Yamaha-Rio3224-D2-a1b2c3"));

    let tx = dante_control::query_tx_channels(arc).await.unwrap();
    assert_eq!(tx.len(), 32, "a Rio3224-D2's 32 inputs");
    assert_eq!(tx[0].number, 1);
    assert_eq!(tx[0].name.as_deref(), Some("01"));
    assert_eq!(tx[31].name.as_deref(), Some("32"));

    // 24 outputs across two pages, none subscribed.
    let rx = dante_control::query_rx_channels(arc).await.unwrap();
    assert_eq!(rx.len(), 24);
    assert_eq!(rx[16].number, 17, "the second page starts where the first stopped");
    assert!(rx.iter().all(|channel| channel.tx_device_name.is_none()));
}

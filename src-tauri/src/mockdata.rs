//! Fixtures for `DUSK_MOCK=1`.
//!
//! These exist so the grid can be built and reviewed on a machine with no
//! Sunshine hosts on the network. They cover every state a card can be in,
//! including the ones that are awkward to reproduce on demand.

use crate::applist::HostApp;
use crate::model::{Activity, Device, PairingState, Reachability, ServerDetails};
use crate::registry::Registry;

fn apps() -> Vec<HostApp> {
    [
        ("881448767", "Desktop", true),
        ("1ota", "Steam Big Picture", false),
    ]
    .into_iter()
    .map(|(id, name, hdr)| HostApp {
        id: id.into(),
        name: name.into(),
        hdr,
    })
    .collect()
}

fn device(id: &str, name: &str, address: &str) -> Device {
    let mut device = Device::new(format!("sunshine:mock-{id}"), name.to_string());
    device.add_address(address);
    device.source.mdns = true;
    device.server = Some(ServerDetails {
        hostname: Some(name.to_string()),
        mac: None,
        app_version: Some("0.23.1".into()),
        local_ip: Some(address.to_string()),
    });
    device
}

pub fn seed(registry: &mut Registry) {
    let mut hosting = device("1", "Workshop", "192.168.1.40");
    hosting.reachability = Reachability::Online { rtt_ms: 3 };
    hosting.pairing = PairingState::Paired;
    hosting.activity = Activity::Hosting {
        app_id: Some("881448767".into()),
        app_name: Some("Desktop".into()),
    };
    hosting.apps = apps();

    let mut ready = device("2", "Attic", "192.168.1.51");
    ready.reachability = Reachability::Online { rtt_ms: 11 };
    ready.pairing = PairingState::Paired;
    ready.activity = Activity::Idle;
    ready.apps = apps();

    // Reached over a VPN, so mDNS never saw it.
    let mut vpn = device("3", "Studio tower", "100.84.2.9");
    vpn.source.mdns = false;
    vpn.source.manual = true;
    vpn.reachability = Reachability::Online { rtt_ms: 42 };
    vpn.pairing = PairingState::NotPaired;
    vpn.activity = Activity::Idle;

    // Online but pairing is unknowable without a client certificate, which is
    // the real state of every machine until M2 lands.
    let mut unknown_pairing = device("4", "Basement NUC", "192.168.1.77");
    unknown_pairing.reachability = Reachability::Online { rtt_ms: 7 };
    unknown_pairing.pairing = PairingState::Unknown;
    unknown_pairing.activity = Activity::Idle;

    let mut offline = device("5", "Old laptop", "192.168.1.88");
    offline.reachability = Reachability::Offline;
    offline.pairing = PairingState::Unknown;
    offline.activity = Activity::Unknown;

    for device in [hosting, ready, vpn, unknown_pairing, offline] {
        registry.insert_prebuilt(device);
    }
}

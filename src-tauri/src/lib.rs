//! Dusk — one device-centric shell over Sunshine and Moonlight.
//!
//! M0/M1 scope: the device grid. Discovery and liveness are real; host
//! control and streaming are the next two milestones.

mod commands;
mod discovery;
mod host;
mod mockdata;
mod model;
mod poller;
mod registry;
mod serverinfo;
mod state;
mod store;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use registry::Registry;
use state::AppState;
use store::Store;

fn mock_mode() -> bool {
    std::env::var("DUSK_MOCK").is_ok_and(|v| v == "1")
}

fn self_name() -> String {
    gethostname::gethostname()
        .into_string()
        .unwrap_or_else(|_| "This machine".into())
}

/// Every address this machine answers on.
///
/// Used to recognise our own Sunshine advertisement, which otherwise arrives
/// over mDNS looking exactly like a remote host.
fn local_addresses() -> HashSet<String> {
    let mut addresses = HashSet::from([
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ]);

    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            for interface in interfaces {
                addresses.insert(interface.ip().to_string());
            }
        }
        Err(err) => {
            // Not fatal: the cost is a duplicate card for this machine, not a
            // broken grid.
            eprintln!("dusk: could not enumerate local interfaces: {err}");
        }
    }
    addresses
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();

            let config_dir = app.path().app_config_dir()?;
            let store_path = store::store_path(&config_dir);
            let store = Store::load(&store_path);

            let mut registry = Registry::new(self_name(), local_addresses());

            // Replay the address book before anything else, so hand-added
            // machines are on screen at launch rather than after a probe.
            for entry in &store.manual {
                registry.upsert_manual(&entry.address, entry.port, entry.name.clone());
            }

            if mock_mode() {
                mockdata::seed(&mut registry);
            }

            let http = reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(1500))
                .build()?;

            let state = Arc::new(AppState::new(
                registry,
                host::detect(),
                store,
                store_path,
                http,
            ));
            app.manage(state.clone());

            // Mock fixtures are fixed state; probing them would only mark
            // them offline, and browsing for services we will not use is
            // noise on the network.
            if !mock_mode() {
                discovery::spawn(handle.clone(), state.clone());
                poller::spawn(handle, state);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::add_manual_device,
            commands::remove_manual_device,
            commands::refresh_now,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Dusk");
}

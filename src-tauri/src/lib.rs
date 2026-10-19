//! Dusk — one device-centric shell over Sunshine and Moonlight.
//!
//! M0/M1 scope: the device grid. Discovery and liveness are real; host
//! control and streaming are the next two milestones.

mod applist;
mod commands;
mod discovery;
mod host;
mod install;
mod mockdata;
mod model;
mod moonlight;
mod poller;
mod registry;
mod serverinfo;
mod state;
mod store;
mod sunshine;
mod tray;

use std::collections::HashSet;
use std::sync::Arc;

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

            // Then the machines moonlight-qt remembers. mDNS only finds what
            // is advertising this second, and a host that is asleep — or
            // simply not publishing — is still one of your machines. This is
            // what keeps Dusk's grid from being a shorter list than the one
            // Moonlight shows for the same set of computers.
            for host in moonlight::hosts::load() {
                registry.upsert_moonlight(&host);
            }

            if mock_mode() {
                mockdata::seed(&mut registry);
            }

            let http = state::plain_client()?;

            // Adopt moonlight-qt's client identity if it has one. Without it
            // every probe is unauthenticated and pairing state stays unknown,
            // which is a degraded grid rather than a broken one.
            let tls = moonlight::identity::load().and_then(|id| state::tls_client(&id));
            if tls.is_none() {
                eprintln!(
                    "dusk: no Moonlight client identity found; pairing state will read as unknown"
                );
            }

            // Only Tauri knows where bundled resources land on each platform,
            // so the client search is handed that directory rather than
            // guessing at it from the executable's path.
            let resources = app.path().resource_dir().ok();
            let moonlight = moonlight::Moonlight::discover(resources.as_deref());
            if moonlight.is_none() {
                eprintln!("dusk: moonlight-qt not found; pairing and streaming are unavailable");
            }

            let state = Arc::new(AppState::new(
                registry,
                host::detect(),
                store,
                store_path,
                http,
                tls,
                moonlight,
            ));
            app.manage(state.clone());

            // Dusk turns Sunshine's own tray icon off, so it owes the person
            // one of its own. Not fatal if the platform will not give us one.
            if let Err(err) = tray::install(&handle) {
                eprintln!("dusk: could not create the tray icon: {err}");
            }

            // Mock fixtures are fixed state; probing them would only mark
            // them offline, and browsing for services we will not use is
            // noise on the network.
            // Read the keystore once, in the background, and push a snapshot
            // when it lands. Deliberately not awaited here: on macOS the read
            // can sit behind a password prompt, and setup running before the
            // window exists would mean opening to an empty grid until someone
            // noticed the prompt and answered it.
            let primed = state.clone();
            let primed_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                primed.credentials.prime().await;
                state::emit_snapshot(&primed_handle, &primed).await;
            });

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
            commands::pair_device,
            commands::launch_app,
            commands::quit_session,
            commands::start_hosting,
            commands::stop_hosting,
            commands::sign_in_host,
            commands::sign_out_host,
            commands::accept_pin,
            commands::get_host_config,
            commands::save_host_config,
            commands::get_setup,
            commands::preview_sunshine_download,
            commands::open_privacy_settings,
            commands::install_sunshine,
            commands::open_firewall,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Dusk");
}

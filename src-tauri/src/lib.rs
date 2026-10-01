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

/// Keep WebKit off its DMA-BUF renderer on Wayland.
///
/// Without this the window never appears on a Wayland compositor that
/// implements explicit sync — KWin does, so every KDE Wayland session, which
/// includes Bazzite and the Steam Deck, two machines a game-streaming app
/// cannot afford to miss. The failure is total and nearly mute: GTK prints
/// one `Error 71 (Protocol error)` line and exits. `WAYLAND_DEBUG=1` names
/// the real cause — `wp_linux_drm_syncobj_surface_v1: explicit sync is used,
/// but no acquire point is set`, webkit2gtk's buffer going up without the
/// acquire point the protocol requires. It is a webkit2gtk bug, so the only
/// lever on this side is to not take that path.
///
/// Scoped to Wayland because the DMA-BUF renderer is the faster one and is
/// perfectly sound under X11, and skipped when already set so anyone
/// debugging the renderer keeps the final say.
#[cfg(target_os = "linux")]
fn avoid_wayland_dmabuf_crash() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some() {
        return;
    }
    // Safety: called from `run` before any window, thread or GTK init, so
    // nothing else can be reading the environment yet.
    unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
}

pub fn run() {
    #[cfg(target_os = "linux")]
    avoid_wayland_dmabuf_crash();

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

            // Which client Dusk will run has to be settled first: it
            // decides which settings store the host list and the identity
            // below are read from. Two Moonlights on one machine keep
            // separate stores, and reading the wrong one makes the grid
            // describe a client Dusk is not going to launch.
            let moonlight = moonlight::Moonlight::discover();
            if moonlight.is_none() {
                eprintln!("dusk: moonlight-qt not found; pairing and streaming are unavailable");
            }

            // Then the machines moonlight-qt remembers. mDNS only finds what
            // is advertising this second, and a host that is asleep — or
            // simply not publishing — is still one of your machines. This is
            // what keeps Dusk's grid from being a shorter list than the one
            // Moonlight shows for the same set of computers.
            for host in moonlight::hosts::load(moonlight.as_ref()) {
                registry.upsert_moonlight(&host);
            }

            if mock_mode() {
                mockdata::seed(&mut registry);
            }

            let http = state::plain_client()?;

            // Adopt moonlight-qt's client identity if it has one. Without it
            // every probe is unauthenticated and pairing state stays unknown,
            // which is a degraded grid rather than a broken one.
            let tls =
                moonlight::identity::load(moonlight.as_ref()).and_then(|id| state::tls_client(&id));
            if tls.is_none() {
                eprintln!(
                    "dusk: no Moonlight client identity found; pairing state will read as unknown"
                );
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

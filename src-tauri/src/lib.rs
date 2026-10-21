//! Dusk — every machine you own in one app, hosting and streaming alike.
//!
//! M0/M1 scope: the device grid. Discovery and liveness are real; host
//! control and streaming are the next two milestones.

mod account;
mod applist;
mod cert_pairing;
mod cli;
mod commands;
mod discovery;
mod host;
mod install;
mod mockdata;
mod model;
mod moonlight;
mod peer;
mod poller;
mod registry;
mod secrets;
mod serverinfo;
mod session;
mod startup;
mod state;
mod store;
mod sunshine;
mod tray;
mod updates;

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

/// Every address another machine could reach this one on.
///
/// `local_addresses` answers a different question — "is this address me?" —
/// and so includes loopback, which is exactly what must not be published.
/// This is the set worth telling other machines about:
///
/// - loopback is never useful to anyone else
/// - link-local (`169.254/16`, `fe80::/10`) does not route
/// - unique-local IPv6 (`fc00::/7`) is what the streaming client already
///   discards, logging `Ignoring ULA:` when it does
///
/// What is left is LAN addresses and overlay-network addresses — Tailscale,
/// ZeroTier and the like — and Dusk deliberately does not try to tell those
/// apart. They are all just addresses to try, and classifying them is how
/// this grows a special case per provider.
pub fn reachable_addresses() -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut addresses: Vec<String> = interfaces
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .filter(|ip| match ip {
            std::net::IpAddr::V4(v4) => !v4.is_link_local(),
            std::net::IpAddr::V6(v6) => {
                let seg = v6.segments()[0];
                // fe80::/10 link-local, fc00::/7 unique-local.
                (seg & 0xffc0) != 0xfe80 && (seg & 0xfe00) != 0xfc00
            }
        })
        .map(|ip| ip.to_string())
        .collect();
    addresses.sort();
    addresses.dedup();
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

/// See [`cli::run_if_asked`]. Runs before any window exists, so a machine
/// with no display can still be set up.
pub fn run_cli_if_asked() -> Option<i32> {
    cli::run_if_asked()
}

/// See [`install::display_driver::run_helper_if_asked`].
pub fn run_helper_if_asked() -> Option<i32> {
    install::display_driver::run_helper_if_asked()
}

/// See [`host::screen_mode::run_hook_if_asked`]. Sunshine runs Dusk around
/// each stream to resize the screen, and that must not open a window: it
/// happens while someone is starting to watch.
pub fn run_screen_hook_if_asked() -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        host::screen_mode::run_hook_if_asked()
    }
    #[cfg(not(target_os = "linux"))]
    None
}

pub fn run() {
    #[cfg(target_os = "linux")]
    avoid_wayland_dmabuf_crash();

    let app = tauri::Builder::default()
        // First, so a second launch hands over before anything else of it
        // starts: clicking Dusk while it sits in the tray brings the window
        // back rather than opening another Dusk beside it.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_window(app)
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg(startup::BACKGROUND_ARG)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                startup::on_close_requested(window, api);
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            updates::ensure_self_executable();
            // Before anything else reads the screen: a stream that ended
            // badly can have left it resized, and this machine's own size is
            // what the grid and the peer answer are about to report.
            #[cfg(target_os = "linux")]
            host::screen_mode::restore_if_left_resized();

            let config_dir = app.path().app_config_dir()?;
            // The CLI has to work this out for itself, having no Tauri to
            // ask. Checked rather than assumed: if the two ever disagree,
            // `dusk screen` writes a setting the window never reads, and
            // silently. Said once here rather than left to be discovered.
            if store::config_dir().as_deref() != Some(config_dir.as_path()) {
                eprintln!(
                    "dusk: the settings directory Tauri gives ({}) is not the one \
                     the command line works out ({:?}); dusk screen will write to \
                     the wrong place",
                    config_dir.display(),
                    store::config_dir(),
                );
            }
            let store_path = store::store_path(&config_dir);
            let store = Store::load(&store_path);

            // The window is created hidden (tauri.conf.json) and shown here,
            // unless this is a sign-in launch that should stay in the tray.
            // Hiding it after the fact would flash it on screen first.
            app.manage(startup::Live::new(&store.startup));
            if startup::starts_hidden(std::env::args(), &store.startup) {
                // Already hidden; this takes the dock icon with it.
                tray::hide_window(&handle);
            } else {
                tray::show_window(&handle);
            }

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
            // Only Tauri knows where bundled resources land on each platform,
            // so the client search is handed that directory rather than
            // guessing at it from the executable's path.
            let resources = app.path().resource_dir().ok();
            let moonlight = moonlight::Moonlight::discover(resources.as_deref());
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
                if !mock_mode() {
                    commands::adopt_host(&primed_handle, &primed).await;
                }
                // After, not alongside: two keystore prompts at once on
                // macOS is one too many to make sense of.
                primed.account.prime(&primed.http).await;
            });

            if !mock_mode() {
                discovery::spawn(handle.clone(), state.clone());
                peer::spawn(handle.clone(), state.clone());
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
            commands::leave_stream,
            commands::start_hosting,
            commands::stop_hosting,
            commands::provision_host,
            commands::accept_pin,
            commands::set_allow_other_devices,
            commands::set_virtual_screen,
            commands::set_resolution,
            commands::set_bitrate,
            commands::set_codec,
            commands::answer_pair_request,
            commands::get_account,
            commands::sign_in_with_github,
            commands::cancel_sign_in,
            commands::sign_out_of_github,
            commands::open_sign_in_page,
            commands::get_host_config,
            commands::save_host_config,
            commands::get_setup,
            commands::preview_sunshine_download,
            commands::open_privacy_settings,
            commands::install_sunshine,
            commands::open_firewall,
            commands::allow_input,
            commands::install_display_driver,
            commands::get_startup,
            commands::set_open_at_login,
            commands::set_startup_prefs,
            updates::update_channel,
            updates::check_deb_update,
            updates::download_deb_update,
            updates::install_deb_update,
            updates::ensure_self_executable,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Dusk");

    app.run(|_app, _event| {
        // Clicking the dock icon of a Dusk whose window is hidden.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            tray::show_window(_app);
        }
    });
}

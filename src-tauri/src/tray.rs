//! Dusk's menu bar / notification area icon.
//!
//! This exists because Sunshine's own tray icon is turned off — see
//! [`crate::sunshine::api::SunshineApi::set_tray`]. Two icons for one thing is
//! the seam Dusk is meant to hide, so taking Sunshine's away means putting
//! something equivalent back: a way to reach the window, and a way to turn
//! hosting on and off without opening it.
//!
//! The icon ships as its own PNG rather than reusing the app icon. An app icon
//! is a tile with a backdrop; a tray icon is a bare mark, and on macOS it is
//! handed over as a *template* image, where only the alpha is used and the
//! system picks the colour so it follows the menu bar through light, dark and
//! a highlighted bar. Passing the tiled app icon there would render as a solid
//! blob.

use std::sync::Arc;

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::model::{HostStatus, Snapshot};
use crate::state::{self, AppState};

/// The mark, at 2x for a Retina menu bar.
const ICON: &[u8] = include_bytes!("../icons/tray.png");

const OPEN: &str = "dusk-open";
const HOSTING: &str = "dusk-hosting";
const QUIT: &str = "dusk-quit";

/// The one menu item whose text is not fixed, kept so snapshots can update it.
///
/// Managed state rather than a global: the alternative is a `static`, and a
/// handle to a window-system object is exactly the kind of thing that should
/// not outlive the app it belongs to.
pub struct Tray {
    hosting: MenuItem<tauri::Wry>,
    /// Held so the icon is not dropped — dropping it removes it from the bar.
    _icon: TrayIcon<tauri::Wry>,
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, OPEN, "Open Dusk", true, None::<&str>)?;
    // Starts in the state a machine with no Sunshine is in; the first snapshot
    // corrects it, which is within a tick of the window appearing.
    let hosting = MenuItem::with_id(app, HOSTING, "Turn hosting on", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit Dusk", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &hosting,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let icon = TrayIconBuilder::with_id("dusk")
        .icon(tauri::image::Image::from_bytes(ICON)?)
        // macOS only, and ignored elsewhere: let the system tint the mark to
        // match the bar rather than drawing our own greys into it.
        .icon_as_template(true)
        .tooltip("Dusk")
        .menu(&menu)
        // The left click opens the window, the way a dock icon would; the
        // menu stays on the right click, which is where it is looked for.
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button, .. } = event {
                if button == tauri::tray::MouseButton::Left {
                    show_window(tray.app_handle());
                }
            }
        })
        .build(app)?;

    app.manage(Tray {
        hosting,
        _icon: icon,
    });
    Ok(())
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        OPEN => show_window(app),
        QUIT => app.exit(0),
        HOSTING => toggle_hosting(app.clone()),
        _ => {}
    }
}

/// Bring the window back, from wherever it went.
///
/// All three of these are needed: a window can be hidden, minimised, or simply
/// behind something, and only the last is fixed by focusing alone.
pub fn show_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

/// Take the window out of the way, taskbar entry and all, without quitting.
/// The tray icon is how it comes back if nothing else brings it.
pub fn hide_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn toggle_hosting(app: AppHandle) {
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let state = state.inner().clone();

    tauri::async_runtime::spawn(async move {
        let running = matches!(
            state.host.state().await.status,
            HostStatus::Installed { running: true, .. }
        );

        let result = if running {
            state.host.stop().await
        } else {
            state.host.start().await
        };

        // Nowhere to put an error: this is a menu, not a screen. The next
        // snapshot shows hosting did not change, and the window says why.
        if let Err(err) = result {
            eprintln!("dusk: could not change hosting from the tray: {err}");
        }
        state::emit_snapshot(&app, &state).await;
    });
}

/// Point the menu at what the snapshot says, so the tray cannot drift from
/// the window. Called from the one place snapshots are pushed.
pub fn reflect(app: &AppHandle, snapshot: &Snapshot) {
    let Some(tray) = app.try_state::<Tray>() else {
        return;
    };

    let (label, enabled) = match &snapshot.host.status {
        HostStatus::Installed { running: true, .. } => ("Turn hosting off", true),
        HostStatus::Installed { running: false, .. } => ("Turn hosting on", true),
        // Nothing to turn on, or we cannot tell. Offering it anyway would be
        // a menu item that can only fail.
        HostStatus::NotInstalled | HostStatus::Unknown { .. } => ("Hosting unavailable", false),
    };

    let _ = tray.hosting.set_text(label);
    let _ = tray.hosting.set_enabled(enabled);
}

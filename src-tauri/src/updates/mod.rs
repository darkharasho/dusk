//! What Dusk's self-update needs from the backend.
//!
//! For four of the five install formats the check, the download and the
//! install are the updater plugin's, driven from the UI; this only says
//! whether they are possible and keeps an updated AppImage runnable. The
//! fifth is the `.deb`, which the plugin cannot install and [`deb`] does
//! instead — through the package manager, off the same manifest and the
//! same signing key.

pub mod deb;

use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

/// How this copy of Dusk updates itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateChannel {
    /// It cannot: a debug build, or a Linux build that is neither an
    /// AppImage nor a package Dusk can hand to `apt`.
    None,
    /// The updater plugin replaces this copy in place.
    Plugin,
    /// The package manager installs it, with one administrator prompt.
    Deb,
}

/// How this copy of Dusk can update itself, if it can.
///
/// A debug build never updates, so `tauri dev` cannot be swapped for a
/// release under the developer.
#[tauri::command]
pub fn update_channel() -> UpdateChannel {
    if cfg!(debug_assertions) {
        return UpdateChannel::None;
    }
    if cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_none() {
        return if deb::is_deb_install() {
            UpdateChannel::Deb
        } else {
            UpdateChannel::None
        };
    }
    UpdateChannel::Plugin
}

/// Look for a newer `.deb`. `None` means this copy is current.
#[tauri::command]
pub async fn check_deb_update(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<deb::Available>, String> {
    deb::check(&state.inner().http).await
}

/// Fetch the `.deb` and check its signature. It is installed later, when
/// someone says so.
#[tauri::command]
pub async fn download_deb_update(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    deb::download(&state.inner().http).await
}

/// Hand the fetched `.deb` to the package manager.
#[tauri::command]
pub async fn install_deb_update() -> Result<(), String> {
    deb::install().await
}

/// Puts back an AppImage's exec bit if an update left it without one.
///
/// A sibling app had one update leave its AppImage non-executable, so the
/// relaunch went nowhere. Asserting the bit costs nothing. Called at startup
/// and by the UI after an update installs, before the relaunch.
#[tauri::command]
pub fn ensure_self_executable() {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let Some(path) = std::env::var_os("APPIMAGE") else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let mut perms = meta.permissions();
        if perms.mode() & 0o111 != 0o111 {
            perms.set_mode(0o755);
            if let Err(e) = std::fs::set_permissions(&path, perms) {
                eprintln!("dusk: could not restore the AppImage's exec bit: {e}");
            }
        }
    }
}

/// Called just before the relaunch that finishes an update, so the new Dusk
/// opens its window: see [`crate::startup::mark_show_next_launch`].
#[tauri::command]
pub fn prepare_restart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    crate::startup::mark_show_next_launch(&dir)
}

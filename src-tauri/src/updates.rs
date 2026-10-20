//! What Dusk's self-update needs from the backend.
//!
//! The check, the download and the install are the updater plugin's, driven
//! from the UI. This only answers whether this copy of Dusk can replace
//! itself at all, and keeps an updated AppImage runnable.

/// Whether this copy of Dusk can install an update over itself.
///
/// On Linux only the AppImage can: a .deb belongs to the package manager,
/// and an update has to arrive through it. A debug build never updates, so
/// `tauri dev` cannot be swapped for a release under the developer.
#[tauri::command]
pub fn updates_supported() -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE").is_some();
    }
    true
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

//! What Dusk's self-update needs from the backend.
//!
//! For four of the five install formats the check, the download and the
//! install are the updater plugin's, driven from the UI; this only says
//! whether they are possible and keeps an updated AppImage runnable. The
//! fifth is the `.deb`, which the plugin cannot install and [`deb`] does
//! instead — through the package manager, off the same manifest and the
//! same signing key.

pub mod deb;
pub mod unattended;

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
/// opens its window: see [`crate::startup::mark_next_launch`].
#[tauri::command]
pub fn prepare_restart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    crate::startup::mark_next_launch(&dir, crate::startup::NextLaunch::Show)
}

/// Called when an update that [`prepare_restart`] was called for fails to
/// install, so the marker does not decide some later launch instead.
#[tauri::command]
pub fn cancel_restart(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Ok(dir) = app.path().app_config_dir() {
        let _ = crate::startup::take_launch_marker(&dir);
    }
}

/// Marks every descriptor above the standard three close-on-exec, so the
/// Dusk spawned by a restart inherits none of them. Only ever called on the
/// way out, so nothing here needs them past the spawn.
///
/// The AppImage runtime keeps its mount alive on a pipe: the fuse process
/// writes into it until the last read end closes, then unmounts and exits.
/// That read end reaches Dusk through `exec`, inheritable, and Tauri's
/// restart spawns the next Dusk with everything inheritable. The old mount
/// then lives as long as the new Dusk does, and the Dusk after that, since
/// each one hands the pipe on: a day of releases left five fuse processes
/// and five mounts under /var/tmp on one machine. Each was `kill`ed by hand
/// on 2026-10-23; from here the pipe breaks as the runtime expects.
pub fn keep_descriptors_from_the_next_dusk() {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return;
        };
        for entry in entries.flatten() {
            let Some(fd) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            if fd <= 2 {
                continue;
            }
            // SAFETY: fcntl on a descriptor this process owns, reading and
            // setting only its close-on-exec flag.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
    }
}

/// Restart into the update that has just been installed: the process
/// plugin's relaunch, after [`keep_descriptors_from_the_next_dusk`], which
/// Tauri's restart has no hook for.
#[tauri::command]
pub fn restart_into_update(app: tauri::AppHandle) {
    keep_descriptors_from_the_next_dusk();
    app.restart()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn cloexec(fd: i32) -> bool {
        // SAFETY: reads a flag of a descriptor the test owns, or of stdin.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn an_inherited_pipe_is_kept_from_the_next_dusk() {
        let mut fds = [0i32; 2];
        // SAFETY: a fresh pipe, closed below.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(!cloexec(fds[0]), "a pipe from libc starts inheritable, as the runtime's is");

        keep_descriptors_from_the_next_dusk();

        assert!(cloexec(fds[0]) && cloexec(fds[1]));
        assert!(!cloexec(0), "stdin still reaches the next Dusk");
        // SAFETY: the pipe above, used by nothing else.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}

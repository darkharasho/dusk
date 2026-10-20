//! Whether Dusk opens when you sign in, how it opens, and what closing its
//! window does.
//!
//! Dusk is worth keeping running even with its window shut: it answers
//! another Dusk's pairing request and keeps the tray's hosting switch to
//! hand. So by default closing the window only hides it, and Dusk can be
//! set to start at sign-in straight into the tray.
//!
//! Opening at sign-in is not stored here. The OS entry (a launch agent, a
//! Run key, an autostart file) is the truth, and is read back each time
//! rather than mirrored, so removing it from the system's own settings is
//! reflected in Dusk's.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::install::embedded;

/// Passed by the sign-in entry, so a Dusk started that way knows it was.
pub const BACKGROUND_ARG: &str = "--background";

/// The two startup choices Dusk keeps itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    /// Opened at sign-in, go straight to the tray instead of showing the
    /// window.
    pub start_in_background: bool,
    /// Closing the window hides it and leaves Dusk in the tray, rather than
    /// quitting.
    pub close_to_tray: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            start_in_background: true,
            close_to_tray: true,
        }
    }
}

/// What the Settings screen shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupView {
    pub open_at_login: bool,
    /// Why opening at sign-in cannot be turned on from where Dusk is now.
    pub open_at_login_blocked: Option<String>,
    #[serde(flatten)]
    pub prefs: Prefs,
}

/// `close_to_tray`, readable from a window event without taking the store's
/// async lock on the UI thread.
pub struct Live {
    close_to_tray: AtomicBool,
}

impl Live {
    pub fn new(prefs: &Prefs) -> Self {
        Self {
            close_to_tray: AtomicBool::new(prefs.close_to_tray),
        }
    }

    pub fn set(&self, prefs: &Prefs) {
        self.close_to_tray
            .store(prefs.close_to_tray, Ordering::Relaxed);
    }

    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray.load(Ordering::Relaxed)
    }
}

/// Whether this launch should leave the window hidden.
pub fn starts_hidden(args: impl IntoIterator<Item = String>, prefs: &Prefs) -> bool {
    prefs.start_in_background && args.into_iter().any(|a| a == BACKGROUND_ARG)
}

pub fn view(app: &AppHandle, prefs: Prefs) -> StartupView {
    StartupView {
        open_at_login: app.autolaunch().is_enabled().unwrap_or(false),
        open_at_login_blocked: blocked().map(str::to_string),
        prefs,
    }
}

pub fn set_open_at_login(app: &AppHandle, on: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    if on {
        if let Some(why) = blocked() {
            return Err(why.to_string());
        }
        // Rewritten rather than left, so an entry from a Dusk that has
        // since moved points at this one.
        let _ = launcher.disable();
        launcher.enable().map_err(|e| e.to_string())
    } else {
        launcher.disable().map_err(|e| e.to_string())
    }
}

/// The sign-in entry names this executable's path, so like the hosting
/// agent it is only written from a Dusk that will still be there.
fn blocked() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?;
    embedded::unsettled(&exe).map(|_| {
        "Move Dusk into your Applications folder and open it from there, then turn this on."
    })
}

/// Hide rather than close, when that is the setting.
pub fn on_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    let keep = app.try_state::<Live>().is_some_and(|l| l.close_to_tray());
    if keep {
        api.prevent_close();
        crate::tray::hide_window(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_a_sign_in_launch_starts_hidden() {
        let prefs = Prefs::default();
        assert!(starts_hidden(args(&["dusk", BACKGROUND_ARG]), &prefs));
        assert!(!starts_hidden(args(&["dusk"]), &prefs));
    }

    #[test]
    fn a_sign_in_launch_shows_the_window_when_asked_to() {
        let prefs = Prefs {
            start_in_background: false,
            ..Prefs::default()
        };
        assert!(!starts_hidden(args(&["dusk", BACKGROUND_ARG]), &prefs));
    }

    #[test]
    fn a_store_from_before_these_settings_gets_the_defaults() {
        let prefs: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs, Prefs::default());
    }
}

//! The host's own clipboard, and whether Dusk can use it on this desktop.
//!
//! Error strings here never contain clipboard text: they are fixed messages.

use std::time::Duration;

/// How often the host loop polls its clipboard for a change.
pub const POLL: Duration = Duration::from_millis(500);

/// Text access to the machine's clipboard.
pub trait LocalClipboard: Send {
    /// The current text, or `None` when the clipboard is empty or holds
    /// something other than text.
    fn read_text(&mut self) -> Option<String>;
    /// Replaces the clipboard text. The error never contains `text`.
    fn write_text(&mut self, text: &str) -> Result<(), String>;
}

/// The system clipboard. On X11 the contents are served only while this
/// process lives, which is fine: it lives as long as the host loop.
pub struct System(arboard::Clipboard);

impl System {
    pub fn open() -> Result<Self, String> {
        arboard::Clipboard::new()
            .map(System)
            .map_err(|_| "the clipboard could not be opened".to_string())
    }
}

impl LocalClipboard for System {
    fn read_text(&mut self) -> Option<String> {
        self.0.get_text().ok()
    }

    fn write_text(&mut self, text: &str) -> Result<(), String> {
        self.0
            .set_text(text)
            .map_err(|_| "the clipboard could not be written".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// Why not, in words shown to the user as-is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unsupported(&'static str),
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const GNOME_WAYLAND: &str =
    "Clipboard sharing isn't available when this machine runs GNOME on Wayland.";

/// GNOME on Wayland has no clipboard protocol a background app can use.
/// An unknown session is assumed supported.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn support_for(session_type: Option<&str>, current_desktop: Option<&str>) -> Support {
    let wayland = session_type.is_some_and(|s| s.trim().eq_ignore_ascii_case("wayland"));
    let gnome = current_desktop.is_some_and(|d| {
        d.split(':')
            .any(|part| part.trim().eq_ignore_ascii_case("GNOME"))
    });
    if wayland && gnome {
        Support::Unsupported(GNOME_WAYLAND)
    } else {
        Support::Supported
    }
}

#[cfg(target_os = "linux")]
pub fn support() -> Support {
    support_for(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn support() -> Support {
    Support::Supported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_on_wayland_is_unsupported() {
        assert!(matches!(
            support_for(Some("wayland"), Some("ubuntu:GNOME")),
            Support::Unsupported(_)
        ));
        assert!(matches!(
            support_for(Some("Wayland"), Some("gnome")),
            Support::Unsupported(_)
        ));
    }

    #[test]
    fn kde_on_wayland_and_gnome_on_x11_are_supported() {
        assert_eq!(
            support_for(Some("wayland"), Some("KDE")),
            Support::Supported
        );
        assert_eq!(support_for(Some("x11"), Some("GNOME")), Support::Supported);
    }

    #[test]
    fn an_unknown_session_is_assumed_supported() {
        assert_eq!(support_for(None, None), Support::Supported);
    }

    #[test]
    #[ignore = "touches the real clipboard"]
    fn system_round_trips_text() {
        let mut c = System::open().expect("open clipboard");
        c.write_text("dusk test").expect("write");
        assert_eq!(c.read_text().as_deref(), Some("dusk test"));
    }
}

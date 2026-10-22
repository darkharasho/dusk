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
    /// A number that changes whenever the clipboard does, where the system
    /// keeps one, so a poll can skip reading the whole text when nothing
    /// changed. `None` when unknown: the text is read every time.
    fn change_count(&mut self) -> Option<u64> {
        None
    }
}

/// A clipboard polled for changes, read only when its change count says
/// it may have changed.
pub struct Watched<C> {
    clipboard: C,
    /// The count at the last read that found text.
    seen: Option<u64>,
}

impl<C: LocalClipboard> Watched<C> {
    pub fn new(clipboard: C) -> Self {
        Self {
            clipboard,
            seen: None,
        }
    }

    /// The current text, or `None` when there is nothing new to look at:
    /// the count is unchanged since the last text read, the clipboard is
    /// empty or not text, or the read failed. A read that found no text
    /// does not record the count, so the next poll tries again.
    pub fn poll(&mut self) -> Option<String> {
        let count = self.clipboard.change_count();
        if count.is_some() && count == self.seen {
            return None;
        }
        let text = self.clipboard.read_text()?;
        self.seen = count;
        Some(text)
    }

    /// Replaces the clipboard text. The error never contains `text`.
    pub fn write_text(&mut self, text: &str) -> Result<(), String> {
        self.clipboard.write_text(text)
    }
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

    /// Windows counts every change to the clipboard. Zero means this
    /// session cannot see it, which is the same as not knowing.
    #[cfg(windows)]
    fn change_count(&mut self) -> Option<u64> {
        // SAFETY: takes nothing and only reads a counter.
        let count =
            unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        (count != 0).then_some(u64::from(count))
    }

    /// The general pasteboard's change count, which is safe to read from
    /// any thread. Looked up the way arboard does: the pasteboard can be
    /// missing (under launchd) although it is documented not to be.
    #[cfg(target_os = "macos")]
    fn change_count(&mut self) -> Option<u64> {
        use objc2::rc::{autoreleasepool, Retained};
        use objc2::{msg_send, ClassType};
        use objc2_app_kit::NSPasteboard;
        autoreleasepool(|_| {
            // SAFETY: `generalPasteboard` takes nothing and returns an
            // `NSPasteboard` or nil.
            let pasteboard: Option<Retained<NSPasteboard>> =
                unsafe { msg_send![NSPasteboard::class(), generalPasteboard] };
            u64::try_from(pasteboard?.changeCount()).ok()
        })
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

    /// A clipboard whose change count the test sets, counting its reads.
    struct Counted {
        count: Option<u64>,
        reads: usize,
    }

    impl LocalClipboard for Counted {
        fn read_text(&mut self) -> Option<String> {
            self.reads += 1;
            Some("text".into())
        }
        fn write_text(&mut self, _text: &str) -> Result<(), String> {
            Ok(())
        }
        fn change_count(&mut self) -> Option<u64> {
            self.count
        }
    }

    fn watched(count: Option<u64>) -> Watched<Counted> {
        Watched::new(Counted { count, reads: 0 })
    }

    #[test]
    fn an_unchanged_count_is_not_read() {
        let mut w = watched(Some(7));
        assert_eq!(w.poll().as_deref(), Some("text"), "the first poll reads");
        assert_eq!(w.poll(), None);
        assert_eq!(w.poll(), None);
        assert_eq!(w.clipboard.reads, 1);
    }

    #[test]
    fn a_changed_count_is_read() {
        let mut w = watched(Some(7));
        w.poll();
        w.clipboard.count = Some(8);
        assert_eq!(w.poll().as_deref(), Some("text"));
        assert_eq!(w.clipboard.reads, 2);
        assert_eq!(w.poll(), None);
    }

    #[test]
    fn an_unknown_count_is_always_read() {
        let mut w = watched(None);
        for _ in 0..3 {
            assert_eq!(w.poll().as_deref(), Some("text"));
        }
        assert_eq!(w.clipboard.reads, 3);
    }

    #[test]
    #[ignore = "touches the real clipboard"]
    fn system_round_trips_text() {
        let mut c = System::open().expect("open clipboard");
        c.write_text("dusk test").expect("write");
        assert_eq!(c.read_text().as_deref(), Some("dusk test"));
    }
}

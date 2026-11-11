//! Watching the host's focused window, so a stream can follow it.
//!
//! A game that turns the camera with the mouse locks the pointer and reads
//! relative motion. Nothing in the stream protocol says when that happens:
//! Sunshine does not know, and on Wayland pointer locking is a request from a
//! client to the compositor that no third party can see. But Dusk runs on the
//! host, so Dusk can ask the compositor what it has in focus.
//!
//! # Shape
//!
//! KWin is the only compositor this supports, because it is the only one
//! with a documented way in: `org.kde.kwin.Scripting` takes a script over
//! D-Bus, and a script can call back out. So Dusk loads
//! [`SCRIPT`] into KWin, owns a name for it to call, turns each report into a
//! [`PointerIntent`], and emits the ones that differ from the last.
//!
//! The script is embedded with `include_str!` and written to a file at load
//! time rather than bundled as a resource, because `loadScript` takes a path
//! and the three platforms disagree about where a bundled file lands. One
//! fewer thing to get wrong per packager.
//!
//! # Not KDE
//!
//! Everything else reports [`Unsupported`](WatchError::Unsupported) with a
//! reason, in the manner of the GNOME Wayland host that turns clipboard
//! sharing off and says why. A host that cannot watch its windows is not
//! broken; it just streams the way it did before.

#![allow(dead_code)]

use super::pointer_intent::{intent_of_with, PointerIntent};

/// The KWin script, embedded so there is no resource path to resolve.
pub const SCRIPT: &str = include_str!("pointer_watch.js");

/// What the script calls, and where.
pub const SERVICE: &str = "dev.dusk.app.PointerWatch";
pub const PATH: &str = "/dev/dusk/app/PointerWatch";
pub const IFACE: &str = "dev.dusk.app.PointerWatch";

/// KWin's scripting interface.
pub const KWIN_SERVICE: &str = "org.kde.KWin";
pub const KWIN_PATH: &str = "/Scripting";
pub const KWIN_IFACE: &str = "org.kde.kwin.Scripting";

/// Why a host is not watching its windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchError {
    /// No compositor this knows how to ask. Carries a reason fit to show.
    Unsupported(String),
    /// KWin is there but would not answer.
    Refused(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::Unsupported(why) => write!(f, "{why}"),
            WatchError::Refused(why) => write!(f, "KWin would not load the watcher: {why}"),
        }
    }
}

/// One report from the script: `"1:<class>"` or `"0:<class>"`.
///
/// Parsed rather than trusted. The script is ours, but it runs inside another
/// process that can be upgraded separately, and a malformed payload must read
/// as "nothing is holding the pointer" rather than as a panic or as a game.
///
/// An empty class with `1:` is a fullscreen window whose class KWin would not
/// give us. That is left to [`intent_of`] to judge, which treats an unknown
/// fullscreen window as a game — see its module docs for why.
pub fn parse_report(payload: &str) -> Option<(bool, &str)> {
    let (flag, class) = payload.split_once(':')?;
    match flag {
        "1" => Some((true, class)),
        "0" => Some((false, class)),
        _ => None,
    }
}

/// The intent a report means, or `Free` if it means nothing.
///
/// Unparseable payloads hand the pointer back rather than taking it: the
/// failure that annoys is a pointer that vanishes, not one that stays.
pub fn intent_of_report(payload: &str, also_denied: &[String]) -> PointerIntent {
    match parse_report(payload) {
        Some((fullscreen, class)) => intent_of_with(class, fullscreen, also_denied),
        None => PointerIntent::Free,
    }
}

/// Remembers the last intent so only changes are acted on.
///
/// The script already drops repeats of the same class-and-fullscreen pair,
/// but two different windows can mean the same intent — alt-tabbing between
/// two fullscreen games should not re-announce anything. This is where that
/// is collapsed, because it is the layer that knows what a class means.
#[derive(Debug, Default)]
pub struct Changes {
    last: Option<PointerIntent>,
}

impl Changes {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// The intent to send, or `None` when it has not changed.
    ///
    /// The first report always sends, even when it is `Free`: a client that
    /// just connected has no idea, and the mode its stream was launched with
    /// may already disagree with what is on screen.
    pub fn next(&mut self, intent: PointerIntent) -> Option<PointerIntent> {
        if self.last == Some(intent) {
            return None;
        }
        self.last = Some(intent);
        Some(intent)
    }

    /// Forget the last intent, so the next report is sent in full.
    ///
    /// Called when a client connects: what the previous client was told is
    /// not what this one knows.
    pub fn forget(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_parses_into_a_flag_and_a_class() {
        assert_eq!(
            parse_report("1:steam_app_5149240"),
            Some((true, "steam_app_5149240"))
        );
        assert_eq!(parse_report("0:firefox"), Some((false, "firefox")));
        assert_eq!(parse_report("0:"), Some((false, "")));
        assert_eq!(parse_report("1:"), Some((true, "")));
    }

    #[test]
    fn a_class_containing_a_colon_keeps_the_rest_of_it() {
        // split_once, not split: a class is not ours to sanitise and a
        // colon in one must not truncate it.
        assert_eq!(parse_report("1:odd:class"), Some((true, "odd:class")));
    }

    #[test]
    fn nonsense_is_read_as_nothing_holding_the_pointer() {
        for payload in ["", "steam", "2:steam", ":steam", "true:steam", "1"] {
            assert_eq!(
                intent_of_report(payload, &[]),
                PointerIntent::Free,
                "{payload:?}"
            );
        }
    }

    #[test]
    fn the_measured_game_launch_reads_as_locked() {
        // The payload the script would have sent for the window captured on
        // the Bazzite host on 2026-11-10.
        assert_eq!(
            intent_of_report("1:steam_app_5149240", &[]),
            PointerIntent::Locked
        );
        // And the focus churn around it reads as free.
        for payload in ["0:steam", "0:sai", "0:com.discordapp.Discord"] {
            assert_eq!(
                intent_of_report(payload, &[]),
                PointerIntent::Free,
                "{payload}"
            );
        }
    }

    #[test]
    fn only_changes_come_out() {
        let mut changes = Changes::new();

        // The first report always sends, even when it is Free.
        assert_eq!(changes.next(PointerIntent::Free), Some(PointerIntent::Free));
        assert_eq!(changes.next(PointerIntent::Free), None);

        assert_eq!(
            changes.next(PointerIntent::Locked),
            Some(PointerIntent::Locked)
        );
        assert_eq!(changes.next(PointerIntent::Locked), None);
        assert_eq!(changes.next(PointerIntent::Free), Some(PointerIntent::Free));
    }

    #[test]
    fn two_different_fullscreen_games_are_one_intent() {
        let mut changes = Changes::new();
        assert_eq!(
            changes.next(intent_of_report("1:steam_app_1", &[])),
            Some(PointerIntent::Locked)
        );
        // Alt-tabbing to another fullscreen game announces nothing: the
        // script sees a different class, this layer sees the same intent.
        assert_eq!(changes.next(intent_of_report("1:steam_app_2", &[])), None);
        assert_eq!(changes.next(intent_of_report("1:factorio", &[])), None);
    }

    #[test]
    fn forgetting_makes_the_next_report_send_again() {
        let mut changes = Changes::new();
        changes.next(PointerIntent::Locked);
        assert_eq!(changes.next(PointerIntent::Locked), None);

        // A client that just connected has to be told what is true now.
        changes.forget();
        assert_eq!(
            changes.next(PointerIntent::Locked),
            Some(PointerIntent::Locked)
        );
    }

    #[test]
    fn the_script_is_embedded_and_says_what_it_calls() {
        // If the names drift apart the script would call into nothing and
        // the pointer would silently never switch, which is the failure mode
        // this whole feature exists to remove.
        assert!(SCRIPT.contains(SERVICE), "service name");
        assert!(SCRIPT.contains(PATH), "object path");
        assert!(SCRIPT.contains(IFACE), "interface");
        assert!(SCRIPT.contains("\"Report\""), "method");

        // And it reads no window titles.
        assert!(
            !SCRIPT.contains(".caption"),
            "the caption is not ours to read"
        );
    }
}

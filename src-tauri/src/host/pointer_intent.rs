//! Whether the host's focused window wants the pointer to itself.
//!
//! The only caller is the KWin observer, which is Linux-specific and lands
//! next, so on every platform this is currently unused — and dead-code
//! analysis does not count the tests below. The allow is about where the
//! first caller is rather than about whether this is real, the same reason
//! [`crate::session::ToClient`] carries one.
#![allow(dead_code)]
//!
//! A game that turns the camera with the mouse locks the pointer and reads
//! relative motion. A client sending absolute positions is ignored by it
//! outright, so the camera does not turn and nothing on either machine says
//! why. This decides, from what the host's compositor reports about its
//! active window, which of the two the stream should be sending.
//!
//! # Why fullscreen and a class, rather than the cursor
//!
//! The signal every other remote-desktop tool uses for this is the host's
//! *cursor*: hidden or locked means relative. It is the better signal — a
//! video player does not hide the cursor and a mouselook game always does —
//! and it is unavailable here. Pointer locking on Wayland is a request from
//! a client to the compositor with no third-party introspection, and KWin
//! does not surface it. So this works from fullscreen plus the window's
//! resource class, which is what KWin will tell us.
//!
//! That makes it a heuristic, and it will eventually be wrong about
//! something. Hence the manual override that outlasts it and the mode being
//! visible in the overlay rather than silent.
//!
//! # Default-allow, not an allow-list
//!
//! Measured on the Bazzite host, a Steam game reports `steam_app_5149240` —
//! the app id, a positive identifier rather than a generic class. It would be
//! tempting to key only on that. But a native game, or one launched through
//! Lutris or Heroic, reports something else entirely, and those are most of
//! the value beyond Steam. So an unknown fullscreen window is treated as a
//! game, and the things that are fullscreen without wanting the pointer are
//! named instead.
//!
//! The cost of that choice is that a fullscreen application nobody listed
//! takes the pointer away until the user overrides it. The cost of the other
//! choice is that the feature silently does nothing for half the games on the
//! machine, which is worse: a feature that does nothing reads as broken,
//! while one that does the wrong thing reads as a thing to turn off.

/// Window classes that are fullscreen without wanting the pointer.
///
/// Compared lowercased, so a class KWin reports as `Steam` matches `steam`.
///
/// `steam` is here and `steam_app_*` is not: Big Picture is fullscreen and
/// pointer-driven, while a game launched through it is neither. That pair is
/// the clearest illustration of why the class matters and fullscreen alone
/// does not.
const NOT_GAMES: &[&str] = &[
    // The Steam client itself, including Big Picture.
    "steam",
    "steamwebhelper",
    // Browsers. A fullscreen video is the false positive this list exists
    // for, and it arrives as a fullscreen browser far more often than as a
    // dedicated player.
    "firefox",
    "org.mozilla.firefox",
    "librewolf",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "brave-browser",
    "microsoft-edge",
    "vivaldi-stable",
    // Dedicated players.
    "mpv",
    "vlc",
    "io.github.celluloid_player.celluloid",
    "org.gnome.totem",
    "kodi",
    // Things people put fullscreen to read or talk, not to play.
    //
    // Telegram is here because it happened, not because it was predicted:
    // within five minutes of the watcher running on the Bazzite host, a
    // fullscreen `org.telegram.desktop` was reported and would have had the
    // pointer taken away from it. The list before that had Discord, Slack
    // and Zoom — which is to say it was a guess at this category from one
    // person's habits. Treat every name below as a known case rather than as
    // coverage, and see the module docs for what is meant to make being
    // wrong survivable.
    "com.discordapp.discord",
    "org.telegram.desktop",
    "telegramdesktop",
    "slack",
    "zoom",
    "element",
    "signal",
    "obs",
    "com.obsproject.studio",
    // Terminals.
    "konsole",
    "org.kde.konsole",
    "alacritty",
    "kitty",
    "org.wezfurlong.wezterm",
    "foot",
    // The desktop itself, and Dusk's own window.
    "plasmashell",
    "dusk",
    "sai",
];

/// What the stream should be sending for the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerIntent {
    /// The focused window wants relative motion: send it, and take the
    /// pointer away from the desktop.
    Locked,
    /// Nothing is claiming the pointer, so it can stay a pointer.
    Free,
}

/// Whether a class is a Steam game: `steam_app_` and then an id.
///
/// The digits are checked rather than just the prefix so that a window
/// someone names `steam_app_launcher` is not read as a game.
fn is_steam_app(class: &str) -> bool {
    match class.strip_prefix("steam_app_") {
        Some(id) => !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// What the focused window wants, from its class and whether it is
/// fullscreen.
///
/// `fullscreen` is the gate: a windowed game is sharing the screen with the
/// desktop, and taking the pointer away from the person using it would be
/// worse than a camera that does not turn. It is also the edge worth
/// triggering on, because focus moves between ordinary windows constantly
/// and none of those movements should change anything.
pub fn intent_of(resource_class: &str, fullscreen: bool) -> PointerIntent {
    if !fullscreen {
        return PointerIntent::Free;
    }

    let class = resource_class.trim().to_ascii_lowercase();

    // Checked before the deny-list so that a game whose id collided with a
    // listed name would still be a game. Nothing collides today; the order
    // states the intent rather than relying on that staying true.
    if is_steam_app(&class) {
        return PointerIntent::Locked;
    }

    if NOT_GAMES.contains(&class.as_str()) {
        return PointerIntent::Free;
    }

    PointerIntent::Locked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classes below were captured from the Bazzite host on 2026-11-10
    /// rather than invented, so this test fails if the real shapes change.
    #[test]
    fn the_captured_run_classifies_the_way_it_looked() {
        // The game, fullscreen: the one case the whole feature is for.
        assert_eq!(intent_of("steam_app_5149240", true), PointerIntent::Locked);

        // Everything else in that run was focus moving between ordinary
        // windows, and none of it should have changed the pointer.
        for class in ["steam", "sai", "com.discordapp.Discord"] {
            assert_eq!(intent_of(class, false), PointerIntent::Free, "{class}");
        }
    }

    #[test]
    fn fullscreen_is_the_gate() {
        // A windowed game shares the screen with the desktop.
        assert_eq!(intent_of("steam_app_5149240", false), PointerIntent::Free);
        // And an unknown window that is not fullscreen is simply a window.
        assert_eq!(intent_of("some-native-game", false), PointerIntent::Free);
    }

    #[test]
    fn big_picture_is_not_a_game_but_what_it_launches_is() {
        // The pair that shows why fullscreen alone is not enough.
        assert_eq!(intent_of("steam", true), PointerIntent::Free);
        assert_eq!(intent_of("Steam", true), PointerIntent::Free);
        assert_eq!(intent_of("steam_app_12345", true), PointerIntent::Locked);
    }

    #[test]
    fn a_fullscreen_video_keeps_the_pointer() {
        // The false positive the deny-list exists for. Unmeasured on
        // hardware: these are the classes those applications are known to
        // report, not ones observed on the Bazzite host.
        for class in [
            "firefox",
            "Firefox",
            "chromium",
            "mpv",
            "vlc",
            "com.discordapp.Discord",
            "konsole",
        ] {
            assert_eq!(intent_of(class, true), PointerIntent::Free, "{class}");
        }
    }

    #[test]
    fn fullscreen_telegram_does_not_take_the_pointer() {
        // Observed on the Bazzite host on 2026-11-10, five minutes into the
        // watcher's first real run: the deny-list had Discord, Slack and
        // Zoom and not this, so a fullscreen chat window would have had the
        // pointer captured. The regression test is the point, not the name.
        assert_eq!(intent_of("org.telegram.desktop", true), PointerIntent::Free);
        assert_eq!(intent_of("telegramdesktop", true), PointerIntent::Free);
    }

    #[test]
    fn an_unknown_fullscreen_window_is_treated_as_a_game() {
        // Default-allow, so a native or Lutris launch is covered without
        // anyone having to name it first.
        for class in ["factorio", "org.libretro.RetroArch", "hl2_linux", ""] {
            assert_eq!(intent_of(class, true), PointerIntent::Locked, "{class:?}");
        }
    }

    #[test]
    fn steam_app_wants_an_actual_id() {
        assert!(is_steam_app("steam_app_1"));
        assert!(is_steam_app("steam_app_5149240"));
        assert!(!is_steam_app("steam_app_"));
        assert!(!is_steam_app("steam_app_launcher"));
        assert!(!is_steam_app("steam_app_12a"));
        assert!(!is_steam_app("steam"));
        assert!(!is_steam_app(""));
    }

    #[test]
    fn whitespace_and_case_do_not_decide_anything() {
        assert_eq!(intent_of("  steam  ", true), PointerIntent::Free);
        assert_eq!(intent_of("KONSOLE", true), PointerIntent::Free);
        assert_eq!(intent_of("  steam_app_99  ", true), PointerIntent::Locked);
    }
}

//! The messages two Dusk machines exchange over the clipboard channel.
//!
//! Each message is one JSON object on one line, so a reader can split the
//! stream on newlines and never has to guess where a message ends. JSON
//! escaping keeps newlines and control characters inside the text from
//! breaking that framing.
//!
//! Only text is carried. A clip with any other `format` is parsed and then
//! ignored by [`text_of`], which is how an older or newer Dusk can add
//! formats later without breaking this one. Unknown message types land in
//! [`Wire::Unknown`] for the same reason.
//!
//! A copy is capped at [`MAX_TEXT_BYTES`] of UTF-8, and [`fits`] is the
//! check the sender makes before sending. Clipboard text is never logged and
//! never shown in a `Debug` output; [`Wire`] prints only the length of a clip.

use serde::{Deserialize, Serialize};

/// The port a Dusk on the host listens on for clipboard sync.
pub const PORT: u16 = 47851;

/// The largest copy that is sent, in UTF-8 bytes.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// The longest line read from the other Dusk: the cap, JSON-escaped (worst
/// case 6×, for control characters written as `\u00XX`), plus framing.
pub const MAX_LINE: usize = MAX_TEXT_BYTES * 6 + 1024;

/// One message on the clipboard channel.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Wire {
    /// The first line each way, carrying the protocol version.
    Hello { version: u32 },
    /// Clipboard text. `format` is `"text"`; anything else is ignored.
    /// Missing fields default to empty, so a clip in a format this build
    /// does not know still parses and is then ignored by [`text_of`].
    Clip {
        #[serde(default)]
        format: String,
        #[serde(default)]
        text: String,
    },
    /// The sender will not take part, and says why.
    Declined { reason: Decline },
    /// The sender is leaving the session.
    Bye,
    /// The host's newest copy is over the cap, so it was not sent. Carries
    /// nothing about the copy. An older Dusk reads it as [`Wire::Unknown`].
    TooBig,
    /// A message type this build does not know. Not an error.
    #[serde(other)]
    Unknown,
}

// Hand-written so a `Wire` in a log line or a panic message never shows the
// clipboard text.
impl std::fmt::Debug for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wire::Hello { version } => f.debug_struct("Hello").field("version", version).finish(),
            Wire::Clip { format, text } => f
                .debug_struct("Clip")
                .field("format", format)
                .field("len", &text.len())
                .finish(),
            Wire::Declined { reason } => {
                f.debug_struct("Declined").field("reason", reason).finish()
            }
            Wire::Bye => f.write_str("Bye"),
            Wire::TooBig => f.write_str("TooBig"),
            Wire::Unknown => f.write_str("Unknown"),
        }
    }
}

/// Why a side will not share, sent in [`Wire::Declined`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decline {
    /// Sharing is turned off on the host.
    SharingOff,
    /// The host is not hosting a stream right now.
    NotStreaming,
    /// The connecting machine is not a paired device.
    NotPaired,
    /// The host's desktop cannot be synced (for example GNOME on Wayland).
    /// `detail` is a fixed human-readable reason, never clipboard text.
    Unsupported { detail: Option<String> },
}

/// Encodes a message as one JSON object followed by a newline.
pub fn line(message: &Wire) -> String {
    // Every `Wire` variant is plain data with string keys, so this cannot fail.
    let mut out = serde_json::to_string(message).expect("wire messages always serialize");
    out.push('\n');
    out
}

/// The text of a clip in the `"text"` format, and nothing else.
pub fn text_of(message: &Wire) -> Option<&str> {
    match message {
        Wire::Clip { format, text } if format == "text" => Some(text),
        _ => None,
    }
}

/// Whether `text` is within the size cap. Measured in bytes, not characters.
pub fn fits(text: &str) -> bool {
    text.len() <= MAX_TEXT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awkward_text_round_trips_on_one_line() {
        for text in [
            "a\r\nb",
            "line1\nline2\n",
            "🦀 ünïcode",
            r#"{"type":"bye"}"#,
            "nul\u{0}here",
        ] {
            let l = line(&Wire::Clip {
                format: "text".into(),
                text: text.into(),
            });
            assert_eq!(l.matches('\n').count(), 1);
            let back: Wire = serde_json::from_str(l.trim_end()).unwrap();
            assert_eq!(text_of(&back), Some(text));
        }
    }

    #[test]
    fn a_format_other_than_text_is_ignored() {
        let w: Wire = serde_json::from_str(r#"{"type":"clip","format":"png","text":""}"#).unwrap();
        assert_eq!(text_of(&w), None);
        let w: Wire =
            serde_json::from_str(r#"{"type":"clip","format":"png","data":"AAAA"}"#).unwrap();
        assert_eq!(text_of(&w), None);
    }

    #[test]
    fn an_unknown_message_is_not_fatal() {
        let w: Wire = serde_json::from_str(r#"{"type":"from_the_future"}"#).unwrap();
        assert!(matches!(w, Wire::Unknown));
    }

    #[test]
    fn too_big_is_a_bare_message_an_older_dusk_ignores() {
        assert_eq!(line(&Wire::TooBig), "{\"type\":\"too_big\"}\n");
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Older {
            Bye,
            #[serde(other)]
            Unknown,
        }
        let older: Older = serde_json::from_str(r#"{"type":"too_big"}"#).unwrap();
        assert!(matches!(older, Older::Unknown));
        let _ = Older::Bye;
    }

    #[test]
    fn the_cap_is_one_megabyte_of_utf8() {
        assert!(fits(&"a".repeat(MAX_TEXT_BYTES)));
        assert!(!fits(&"a".repeat(MAX_TEXT_BYTES + 1)));
        assert!(
            !fits(&"é".repeat(MAX_TEXT_BYTES / 2 + 1)),
            "measured in bytes, not chars"
        );
    }

    #[test]
    fn debug_never_shows_clipboard_text() {
        let w = Wire::Clip {
            format: "text".into(),
            text: "hunter2".into(),
        };
        assert!(!format!("{w:?}").contains("hunter2"));
    }
}

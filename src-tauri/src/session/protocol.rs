//! What Dusk and the stream client say to each other.
//!
//! Newline-delimited JSON over a local socket. One object per line, in both
//! directions, so either side can be read with a line reader and neither
//! needs a framing layer.
//!
//! # Why a protocol at all
//!
//! The client is a separate GPL-3.0 program and has to stay one — see
//! `docs/architecture.md`. Driving it by command line arguments was enough
//! to start a stream and nothing more: the overlay could raise a hotkey menu
//! but had no way to tell Dusk anything, and Dusk had no way to change a
//! session in flight. A documented protocol spoken between two programs is
//! also the textbook case for aggregation rather than a derivative work, so
//! this strengthens the licence boundary rather than straining it.
//!
//! # Trust
//!
//! The socket is per-session and the client proves itself with a one-time
//! token before anything it says is acted on. A unix socket's permissions
//! already restrict it to this user, so the token is not the only thing
//! standing between a session and the rest of the machine — but anything
//! running as this user can reach the socket, and "the first thing to
//! connect wins" is not a property worth having.

use serde::{Deserialize, Serialize};

/// Everything the client can say.
///
/// `deny_unknown_fields` is deliberately *not* set: a newer client sending a
/// field this build does not know about should be ignored, not treated as a
/// protocol error. The reverse — an unknown message *type* — is handled by
/// the `Unknown` fallback rather than by failing the connection.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromClient {
    /// First message on every connection. Nothing else is acted on until
    /// this arrives with the right token.
    Ready {
        token: String,
    },
    Stats(Stats),
    /// The person used the in-stream overlay.
    OverlayAction {
        action: OverlayAction,
    },
    /// The client is ending the session and says why.
    Disconnected {
        reason: Option<String>,
    },
    /// A message type this build does not know. Kept rather than rejected so
    /// a newer client talking to an older Dusk degrades instead of dying.
    #[serde(other)]
    Unknown,
}

/// What the overlay offers. Deliberately intents rather than commands: the
/// overlay raises what the person asked for and Dusk decides what it means,
/// which is what keeps the policy on this side of the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayAction {
    /// End the session and come back to Dusk.
    Disconnect,
    /// Bring Dusk's window up on this machine's settings.
    Settings,
    /// Ask Dusk for a different quality; Dusk answers with `SetQuality`.
    Quality,
}

/// How the stream is doing, as the client measures it.
///
/// Every field is optional because a renderer that cannot measure one should
/// send the rest rather than nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Frames rendered per second.
    pub fps: Option<f32>,
    /// Video bitrate, not counting error correction.
    pub bitrate_kbps: Option<u32>,
    /// Network round trip to the host.
    pub rtt_ms: Option<f32>,
    /// Average time to decode one frame.
    pub decode_ms: Option<f32>,
    /// Percentage of frames the network dropped, 0 to 100.
    pub packet_loss: Option<f32>,
}

/// Everything Dusk can say.
///
/// Most of these have no caller yet: the protocol is defined in full here so
/// the fork has something fixed to implement against, and Dusk starts driving
/// each one as the client learns to answer it. Dead-code analysis does not
/// count the round-trip test, so the allow is about where the first caller
/// is, not about whether the variant is real.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToClient {
    SetQuality {
        bitrate_kbps: Option<u32>,
        fps: Option<u32>,
        resolution: Option<(u32, u32)>,
    },
    /// End the session from this side.
    Disconnect,
    ShowOverlay,
    /// Switch which of the host's displays is being streamed.
    HostDisplay {
        id: String,
    },
}

/// What a received line means for the session.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Nothing to do. Unknown messages and duplicate handshakes land here.
    Ignore,
    /// The client proved itself; it may now be acted on.
    Authenticated,
    /// Shut the connection down. The string is for the log, not for a person.
    Close(String),
    Stats(Stats),
    Overlay(OverlayAction),
    /// The client says the session is over.
    Ended {
        reason: Option<String>,
    },
}

/// One connection's state, as a state machine so it can be tested without a
/// socket. The transport reads lines and feeds them here; everything that
/// decides anything lives in `accept`.
#[derive(Debug)]
pub struct Conversation {
    token: String,
    authenticated: bool,
}

impl Conversation {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            authenticated: false,
        }
    }

    pub fn authenticated(&self) -> bool {
        self.authenticated
    }

    /// Interpret one line.
    pub fn accept(&mut self, line: &str) -> Effect {
        // A blank line is not worth a diagnosis; some writers end with one.
        if line.trim().is_empty() {
            return Effect::Ignore;
        }

        let message: FromClient = match serde_json::from_str(line) {
            Ok(message) => message,
            // Malformed input closes the connection rather than being
            // skipped: a desynchronised stream of lines will not recover on
            // its own, and continuing would mean acting on half a protocol.
            Err(err) => return Effect::Close(format!("could not read a message: {err}")),
        };

        match message {
            FromClient::Ready { token } => {
                if self.authenticated {
                    // Harmless, and not worth dropping a live session over.
                    return Effect::Ignore;
                }
                // Constant-time would be the reflex, but this token lives for
                // one session on a socket only this user can open, and an
                // attacker who can time it can also just read the socket.
                if token != self.token {
                    return Effect::Close("the client offered the wrong token".into());
                }
                self.authenticated = true;
                Effect::Authenticated
            }
            // Everything below needs the handshake first. Silence rather
            // than an error: an unauthenticated peer learns nothing from
            // being told which of its guesses was wrong.
            _ if !self.authenticated => {
                Effect::Close("the client spoke before proving itself".into())
            }
            FromClient::Stats(stats) => Effect::Stats(stats),
            FromClient::OverlayAction { action } => Effect::Overlay(action),
            FromClient::Disconnected { reason } => Effect::Ended { reason },
            FromClient::Unknown => Effect::Ignore,
        }
    }
}

/// Serialise one message, newline included, ready to write.
pub fn line(message: &ToClient) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string(message)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(token: &str) -> String {
        format!(r#"{{"type":"ready","token":"{token}"}}"#)
    }

    #[test]
    fn nothing_is_acted_on_before_the_handshake() {
        let mut c = Conversation::new("secret");
        let effect = c.accept(r#"{"type":"overlay_action","action":"disconnect"}"#);
        assert_eq!(
            effect,
            Effect::Close("the client spoke before proving itself".into()),
            "an unauthenticated peer must not be able to end a session"
        );
        assert!(!c.authenticated());
    }

    #[test]
    fn the_wrong_token_closes_the_connection() {
        let mut c = Conversation::new("secret");
        assert!(matches!(c.accept(&ready("guess")), Effect::Close(_)));
        assert!(!c.authenticated());
    }

    #[test]
    fn the_right_token_opens_it() {
        let mut c = Conversation::new("secret");
        assert_eq!(c.accept(&ready("secret")), Effect::Authenticated);
        assert!(c.authenticated());
    }

    #[test]
    fn a_second_handshake_is_ignored_rather_than_fatal() {
        // Not worth dropping a live stream over, and a reconnecting client
        // repeating itself is the likeliest cause.
        let mut c = Conversation::new("secret");
        c.accept(&ready("secret"));
        assert_eq!(c.accept(&ready("secret")), Effect::Ignore);
        assert!(c.authenticated());
    }

    #[test]
    fn a_repeated_handshake_cannot_be_used_to_re_authenticate_with_a_bad_token() {
        let mut c = Conversation::new("secret");
        c.accept(&ready("secret"));
        // Still ignored, still authenticated — but importantly the bad token
        // does not *un*-authenticate, nor does it authenticate anything new.
        assert_eq!(c.accept(&ready("wrong")), Effect::Ignore);
        assert!(c.authenticated());
    }

    #[test]
    fn stats_come_through_with_whatever_the_renderer_could_measure() {
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        let effect = c.accept(r#"{"type":"stats","fps":59.5,"bitrateKbps":18000}"#);
        assert_eq!(
            effect,
            Effect::Stats(Stats {
                fps: Some(59.5),
                bitrate_kbps: Some(18000),
                ..Default::default()
            }),
            "a renderer that cannot measure a field should send the rest"
        );
    }

    #[test]
    fn an_overlay_action_arrives_as_an_intent() {
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        assert_eq!(
            c.accept(r#"{"type":"overlay_action","action":"settings"}"#),
            Effect::Overlay(OverlayAction::Settings)
        );
    }

    #[test]
    fn a_newer_clients_unknown_message_is_ignored_not_fatal() {
        // The whole point of the Unknown fallback: a client from a later
        // release should degrade, not kill the session.
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        assert_eq!(
            c.accept(r#"{"type":"something_invented_later","detail":7}"#),
            Effect::Ignore
        );
    }

    #[test]
    fn an_unknown_field_on_a_known_message_is_ignored_too() {
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        assert_eq!(
            c.accept(r#"{"type":"stats","fps":60.0,"somethingNew":1}"#),
            Effect::Stats(Stats {
                fps: Some(60.0),
                ..Default::default()
            })
        );
    }

    #[test]
    fn malformed_json_closes_rather_than_being_skipped() {
        // A desynchronised line stream does not recover on its own, and
        // skipping would mean acting on half a protocol.
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        assert!(matches!(c.accept("{not json"), Effect::Close(_)));
    }

    #[test]
    fn blank_lines_are_not_an_error() {
        let mut c = Conversation::new("t");
        assert_eq!(c.accept(""), Effect::Ignore);
        assert_eq!(c.accept("   "), Effect::Ignore);
    }

    #[test]
    fn the_client_can_say_why_it_ended() {
        let mut c = Conversation::new("t");
        c.accept(&ready("t"));
        assert_eq!(
            c.accept(r#"{"type":"disconnected","reason":"the host went away"}"#),
            Effect::Ended {
                reason: Some("the host went away".into())
            }
        );
        // And may say nothing.
        assert_eq!(
            c.accept(r#"{"type":"disconnected"}"#),
            Effect::Ended { reason: None }
        );
    }

    #[test]
    fn outgoing_messages_are_one_line_each() {
        let rendered = line(&ToClient::SetQuality {
            bitrate_kbps: Some(20000),
            fps: Some(60),
            resolution: Some((1920, 1080)),
        })
        .unwrap();

        assert!(rendered.ends_with('\n'));
        assert_eq!(
            rendered.matches('\n').count(),
            1,
            "a message has to be exactly one line, or the reader desynchronises"
        );
        assert!(rendered.contains(r#""type":"set_quality""#));
    }

    #[test]
    fn every_outgoing_message_round_trips_as_one_line() {
        let messages = [
            ToClient::Disconnect,
            ToClient::ShowOverlay,
            ToClient::HostDisplay { id: "1".into() },
            ToClient::SetQuality {
                bitrate_kbps: None,
                fps: None,
                resolution: None,
            },
        ];
        for message in messages {
            let rendered = line(&message).unwrap();
            assert_eq!(rendered.matches('\n').count(), 1, "{message:?}");
            serde_json::from_str::<serde_json::Value>(rendered.trim()).expect("valid JSON");
        }
    }
}

//! Text clipboard sync between two Dusk machines while a stream runs.
//!
//! While a stream is running, text copied on either machine can be pasted on
//! the other. The host side listens on [`wire::PORT`] and the client side
//! connects to it, over the same TLS connection pinned to the exact
//! certificates each side already trusts. Nothing is sent when the stream
//! starts: each side reads its clipboard once at connection start as a
//! baseline, and only a copy that differs from that baseline is sent.
//!
//! The messages are one JSON line each (see [`wire`]). Text only, capped at
//! 1 MB of UTF-8 per copy; a larger copy is not sent, and the overlay says so.
//! [`guard`] keeps a copy that arrived from the other machine from being sent
//! straight back.
//!
//! Clipboard text is never logged, never written to disk, and never put in an
//! error string, so a bug report cannot leak what was copied.

pub mod client;
pub mod guard;
pub mod host;
pub mod local;
pub mod tls;
pub mod wire;

pub use guard::Guard;
pub use wire::{fits, line, text_of, Decline, Wire, MAX_LINE, PORT};

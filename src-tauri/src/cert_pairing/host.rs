//! The host's side of pairing by certificate.

use std::sync::Arc;

use crate::account::Proof;
use crate::peer::Message;
use crate::state::AppState;

/// Answer a `pair_cert` request. Not wired up yet: every request is declined.
pub async fn handle(
    _state: &Arc<AppState>,
    _address: &str,
    _name: String,
    _cert: String,
    _proof: Proof,
) -> Message {
    Message::Declined {
        reason: "This machine can't pair that way yet.".into(),
    }
}

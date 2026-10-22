//! Stops clipboard sync from bouncing text between the two machines.
//!
//! Each side watches its own clipboard, so without a guard a copy on A is
//! sent to B, written to B's clipboard, seen as a new copy by B, and sent
//! back. The guard remembers hashes, never the text itself, of three things:
//! the clipboard as it was when the connection started (the baseline), the
//! last copy this side sent, and the last copy this side wrote from the
//! other machine. A copy matching any of them is not sent.
//!
//! Hashes are enough because the guard only ever asks "is this the same as
//! that?", so clipboard text is never kept here after the caller drops it.

use sha2::{Digest, Sha256};

type Hash = [u8; 32];

fn hash(text: &str) -> Hash {
    Sha256::digest(text.as_bytes()).into()
}

/// Decides which clipboard changes should cross the connection.
pub struct Guard {
    baseline: Option<Hash>,
    last_sent: Option<Hash>,
    last_applied: Option<Hash>,
}

impl Guard {
    /// Starts a connection with the clipboard as it was when it opened.
    pub fn new(baseline: Option<&str>) -> Self {
        Self {
            baseline: baseline.map(hash),
            last_sent: None,
            last_applied: None,
        }
    }

    /// Local clipboard now reads `text`: should it be sent? Records it if so.
    pub fn outgoing(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let h = hash(text);
        if self.baseline == Some(h) || self.last_sent == Some(h) || self.last_applied == Some(h) {
            return false;
        }
        self.last_sent = Some(h);
        true
    }

    /// `text` arrived from the other side: should it be written locally?
    /// Records it if so.
    pub fn incoming(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let h = hash(text);
        if self.last_applied == Some(h) {
            return false;
        }
        self.last_applied = Some(h);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_does_not_send_the_baseline() {
        let mut g = Guard::new(Some("old"));
        assert!(!g.outgoing("old"));
        assert!(g.outgoing("new"));
    }

    #[test]
    fn applied_text_is_not_sent_back() {
        let mut g = Guard::new(None);
        assert!(g.incoming("from B"));
        assert!(!g.outgoing("from B"));
    }

    #[test]
    fn the_same_copy_is_sent_once() {
        let mut g = Guard::new(None);
        assert!(g.outgoing("a"));
        assert!(!g.outgoing("a"));
        assert!(g.outgoing("b"));
        assert!(g.outgoing("a"), "copying a again after b is a new copy");
    }

    #[test]
    fn empty_text_is_never_sent() {
        let mut g = Guard::new(None);
        assert!(!g.outgoing(""));
        assert!(!g.incoming(""));
    }
}

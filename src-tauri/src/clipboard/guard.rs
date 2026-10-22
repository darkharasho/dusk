//! Stops clipboard sync from bouncing text between the two machines.
//!
//! Each side watches its own clipboard, so without a guard a copy on A is
//! sent to B, written to B's clipboard, seen as a new copy by B, and sent
//! back. The guard remembers one hash, never the text itself: the last text
//! that crossed the connection in either direction, starting from the
//! clipboard as it was when the connection opened (the baseline). A copy
//! equal to that hash is not sent or applied. Any other copy is, and becomes
//! the new hash, so the most recent change on either side wins.
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
    last_synced: Option<Hash>,
}

impl Guard {
    /// Starts a connection with the clipboard as it was when it opened.
    pub fn new(baseline: Option<&str>) -> Self {
        Self {
            last_synced: baseline.map(hash),
        }
    }

    /// Local clipboard now reads `text`: should it be sent? Records it if so.
    pub fn outgoing(&mut self, text: &str) -> bool {
        self.record(text)
    }

    /// `text` arrived from the other side: should it be written locally?
    /// Records it if so.
    pub fn incoming(&mut self, text: &str) -> bool {
        self.record(text)
    }

    fn record(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let h = hash(text);
        if self.last_synced == Some(h) {
            return false;
        }
        self.last_synced = Some(h);
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

    #[test]
    fn a_copy_after_the_other_side_wrote_is_sent() {
        let mut g = Guard::new(None);
        assert!(g.outgoing("x"));
        assert!(g.incoming("y"));
        assert!(
            g.outgoing("x"),
            "the user copied x again after the other side wrote y"
        );
    }

    #[test]
    fn a_remote_copy_after_a_local_one_is_applied() {
        let mut g = Guard::new(None);
        assert!(g.incoming("x"));
        assert!(g.outgoing("y"));
        assert!(
            g.incoming("x"),
            "the other side copied x again after this side copied y"
        );
    }
}

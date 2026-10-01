//! Where Sunshine's web-UI password lives.
//!
//! This is a real credential — it authorises changing the host's entire
//! configuration — so it goes in the OS keystore, not a JSON file next to
//! the address book. `keyring` maps onto the Keychain, Credential Manager
//! and Secret Service respectively.
//!
//! Secret Service is the one that can legitimately be missing: a headless or
//! minimal Linux session may have no keyring daemon at all. Dusk does not
//! fall back to writing the password to disk in that case. It keeps it for
//! the run and says so, because a silent downgrade from "encrypted at rest"
//! to "plaintext in your home directory" is exactly the kind of thing nobody
//! finds out about until it matters.
//!
//! # The keystore is read once per run, and that is load-bearing
//!
//! `load` is called from `AppState::snapshot`, and a snapshot is built on
//! every poll tick and every discovery event — several times a minute. Read
//! through to the keystore each time and macOS puts a Keychain authorisation
//! prompt on screen every few seconds, which is unusable. Worse, it never
//! settles in development: the Keychain grants access to a *binary*, and an
//! unsigned one has a new identity after every `cargo build`, so "Always
//! Allow" is revoked by the next rebuild.
//!
//! So the keystore is consulted at most once per run and the answer is kept
//! in memory. Writes go through `save`/`forget`, which update both, so the
//! cache cannot drift from the keystore by any path Dusk controls.

use tokio::sync::RwLock;

const SERVICE: &str = "dev.dusk.app";
const ACCOUNT: &str = "sunshine-local-api";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// How the keystore is read. A field rather than a direct call so a test can
/// count the reads, which is the one property worth pinning here.
type Reader = Box<dyn Fn() -> Option<Credentials> + Send + Sync>;

#[derive(Default)]
struct Cached {
    /// False until the keystore has been consulted this run.
    read: bool,
    credentials: Option<Credentials>,
    /// False when the credential is held for this run only, because the
    /// keystore could not be written.
    persistent: bool,
}

pub struct CredentialStore {
    cached: RwLock<Cached>,
    reader: Reader,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self {
            cached: RwLock::new(Cached {
                read: false,
                credentials: None,
                // Nothing is held yet, so nothing is at risk of being lost.
                persistent: true,
            }),
            reader: Box::new(read_keystore),
        }
    }
}

impl CredentialStore {
    pub async fn load(&self) -> Option<Credentials> {
        {
            let cached = self.cached.read().await;
            if cached.read {
                return cached.credentials.clone();
            }
        }

        let mut cached = self.cached.write().await;
        // Another task may have done the read while we waited for the lock.
        if !cached.read {
            cached.credentials = (self.reader)();
            cached.read = true;
        }
        cached.credentials.clone()
    }

    pub async fn save(&self, credentials: Credentials) -> Result<(), String> {
        let mut cached = self.cached.write().await;
        cached.persistent = match to_keystore(&credentials) {
            Ok(()) => true,
            Err(err) => {
                eprintln!(
                    "dusk: keystore unavailable ({err}); holding credentials for this run only"
                );
                false
            }
        };
        cached.credentials = Some(credentials);
        cached.read = true;
        Ok(())
    }

    pub async fn forget(&self) {
        let mut cached = self.cached.write().await;
        cached.credentials = None;
        cached.read = true;
        cached.persistent = true;
        if let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) {
            // A missing entry is the desired end state, not a failure.
            let _ = entry.delete_credential();
        }
    }

    /// False when the password is only held in memory, so the UI can say
    /// that it will be asked for again next launch.
    pub async fn is_persistent(&self) -> bool {
        self.cached.read().await.persistent
    }
}

fn read_keystore() -> Option<Credentials> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).ok()?;
    let raw = entry.get_password().ok()?;
    // Stored as `username\npassword`: the keystore holds one secret per
    // entry, and the username is not sensitive enough to need a second.
    let (username, password) = raw.split_once('\n')?;
    Some(Credentials {
        username: username.to_string(),
        password: password.to_string(),
    })
}

fn to_keystore(credentials: &Credentials) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())?;
    entry
        .set_password(&format!(
            "{}\n{}",
            credentials.username, credentials.password
        ))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A store whose keystore reads are counted. The counter is per-store
    /// rather than global so these can run in parallel with each other.
    fn store() -> (CredentialStore, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let store = CredentialStore {
            reader: Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Some(Credentials {
                    username: "sunshine".into(),
                    password: "hunter2".into(),
                })
            }),
            ..Default::default()
        };
        (store, reads)
    }

    #[tokio::test]
    async fn the_keystore_is_read_at_most_once_however_often_load_is_called() {
        // A snapshot is built every poll tick and calls this. Reading through
        // each time put a Keychain prompt on screen every few seconds.
        let (store, reads) = store();
        for _ in 0..25 {
            assert_eq!(store.load().await.unwrap().username, "sunshine");
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn signing_out_is_remembered_without_going_back_to_the_keystore() {
        // Otherwise the next snapshot reads the deleted entry and, on a
        // keystore that prompts, asks again for something already discarded.
        let (store, reads) = store();
        store.load().await;
        store.forget().await;
        assert!(store.load().await.is_none());
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_session_fallback_is_readable_but_not_persistent() {
        // A keystore that cannot be written: the credential still works for
        // the run, but must not claim to have been saved.
        let (store, _) = store();
        let mut cached = store.cached.write().await;
        cached.persistent = false;
        cached.credentials = Some(Credentials {
            username: "sunshine".into(),
            password: "hunter2".into(),
        });
        cached.read = true;
        drop(cached);

        assert_eq!(store.load().await.unwrap().username, "sunshine");
        assert!(
            !store.is_persistent().await,
            "a session-only credential must not claim to be saved"
        );
    }

    #[test]
    fn the_stored_form_round_trips_a_password_containing_a_colon() {
        // Newline-separated rather than colon-separated precisely so that a
        // password with a colon in it survives.
        let raw = "sunshine\npa:ss:word";
        let (user, pass) = raw.split_once('\n').unwrap();
        assert_eq!(user, "sunshine");
        assert_eq!(pass, "pa:ss:word");
    }
}

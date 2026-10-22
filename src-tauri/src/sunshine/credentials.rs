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
//! # A keystore read is slow, blocking, and may put a dialog on screen
//!
//! Two things follow from that, and both were learned the hard way.
//!
//! **It cannot run on the async runtime.** `keyring` is synchronous, and on
//! macOS a read can sit for as long as it takes someone to notice a password
//! prompt and type into it. Awaiting that on a runtime worker stalls whatever
//! else that worker owns. Every keystore call here goes through
//! `spawn_blocking`.
//!
//! **Nothing on the render path may wait for it.** `AppState::snapshot` is
//! built on every poll tick and is what the grid is drawn from. When it read
//! through to the keystore, the first snapshot of the run — the one the
//! window has nothing to show without — blocked behind that prompt, so Dusk
//! opened to an empty window until the prompt was answered. Dismiss the
//! prompt and the read failed, which presented as "not signed in" from a
//! machine whose password was sitting in the Keychain the whole time.
//!
//! So there are two accessors and the distinction between them is the point:
//! [`CredentialStore::peek`] never touches the keystore and is what snapshots
//! use, while [`CredentialStore::load`] may block and is only called when
//! something genuinely needs the secret. Until the one read of the run lands,
//! `peek` answers [`Known::Unknown`] and the UI says so rather than guessing.
//!
//! # The read happens at most once per run, and that is load-bearing
//!
//! Read through on every snapshot and macOS puts a Keychain prompt on screen
//! every few seconds, which is unusable. Worse, it never settles in
//! development: the Keychain grants access to a *binary*, and an unsigned one
//! has a new identity after every `cargo build`, so "Always Allow" is revoked
//! by the next rebuild.
//!
//! So the keystore is consulted once and the answer is kept in memory. Writes
//! go through `save`, which updates both, so the cache cannot drift
//! from the keystore by any path Dusk controls. A read that *fails* is not an
//! answer and is not cached — otherwise one dismissed prompt would strand the
//! run believing there is no password.

use std::sync::Arc;

use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// What is known about the sign-in without consulting the keystore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    /// The keystore has not been read yet, and reading it here would block.
    Unknown,
    SignedIn,
    SignedOut,
}

/// How the keystore is read.
///
/// `Ok(None)` is a definitive "there is nothing stored"; `Err` is "the
/// keystore could not answer", which is not the same thing and must not be
/// remembered as one. A field rather than a direct call so a test can count
/// the reads, which is the one property worth pinning here.
/// `Arc` rather than `Box` so it can be cloned onto the blocking pool.
type Reader = Arc<dyn Fn() -> Result<Option<Credentials>, String> + Send + Sync>;

/// How the keystore is written.
///
/// Injectable for the same reason the reader is, and that is not only about
/// observability. When only reads were stubbed, a test reached the real
/// keystore and deleted the developer's own saved Sunshine password — which
/// then looks exactly like a password that never saved. Every path to the
/// keystore goes through one of these fields, so a test cannot reach the
/// real one by accident.
type Writer = Arc<dyn Fn(&Credentials) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
struct Cached {
    /// False until the keystore has given a definitive answer this run.
    read: bool,
    credentials: Option<Credentials>,
    /// False when the credential is held for this run only, because the
    /// keystore could not be written.
    persistent: bool,
}

pub struct CredentialStore {
    cached: RwLock<Cached>,
    reader: Reader,
    writer: Writer,
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
            reader: Arc::new(read_keystore),
            writer: Arc::new(to_keystore),
        }
    }
}

impl CredentialStore {
    /// What is known right now, without touching the keystore.
    ///
    /// Safe to call from anything that must not block — which is anything on
    /// the path to drawing a window.
    pub async fn peek(&self) -> Known {
        let cached = self.cached.read().await;
        match (cached.read, &cached.credentials) {
            (false, None) => Known::Unknown,
            (_, Some(_)) => Known::SignedIn,
            (true, None) => Known::SignedOut,
        }
    }

    /// The credential, reading the keystore if this is the first time.
    ///
    /// May block for as long as it takes someone to answer a Keychain prompt,
    /// so call it only where that wait belongs: something the person just
    /// asked for, not a background refresh.
    pub async fn load(&self) -> Option<Credentials> {
        match self.try_load().await {
            Ok(credentials) => credentials,
            // Not an answer, so it is not remembered as one. The next thing
            // that needs the password will ask the keystore again.
            Err(err) => {
                eprintln!("dusk: could not read the keystore ({err}); will try again when needed");
                None
            }
        }
    }

    /// Like [`load`](Self::load), but a keystore that would not answer is
    /// an `Err`, distinct from a definite "nothing is stored".
    ///
    /// Anything that would *change* the host on finding no password (set a
    /// new one, restart the service) must use this one: a locked keyring
    /// is not a sign-out, and treating it as one rotates the password of a
    /// host that was working.
    pub async fn try_load(&self) -> Result<Option<Credentials>, String> {
        {
            let cached = self.cached.read().await;
            if cached.read {
                return Ok(cached.credentials.clone());
            }
        }

        let mut cached = self.cached.write().await;
        // Another task may have done the read while we waited for the lock.
        if cached.read {
            return Ok(cached.credentials.clone());
        }

        let credentials = self.read_off_runtime().await?;
        cached.credentials = credentials.clone();
        cached.read = true;
        Ok(credentials)
    }

    /// Do the one read of the run in the background, off the render path.
    ///
    /// Called at startup so the grid can stop saying "checking" without any
    /// snapshot ever having waited on the keystore.
    pub async fn prime(&self) {
        self.load().await;
    }

    pub async fn save(&self, credentials: Credentials) -> Result<(), String> {
        let written = {
            let to_write = credentials.clone();
            let writer = self.writer.clone();
            spawn_blocking(move || writer(&to_write)).await
        };

        let mut cached = self.cached.write().await;
        cached.persistent = match written {
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

    /// False when the password is only held in memory, so the UI can say
    /// that it will be asked for again next launch.
    pub async fn is_persistent(&self) -> bool {
        self.cached.read().await.persistent
    }

    /// Run the reader on the blocking pool.
    ///
    /// `block_in_place` would be the tidier shape but panics on a
    /// current-thread runtime, which is what the tests use.
    async fn read_off_runtime(&self) -> Result<Option<Credentials>, String> {
        let reader = self.reader.clone();
        spawn_blocking(move || reader()).await
    }
}

/// Run a blocking keystore call off the async runtime.
///
/// A panic in the closure (a keyring backend aborting, say) comes back as an
/// error rather than taking the task down with it.
async fn spawn_blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(err) => Err(format!("keystore call did not finish: {err}")),
    }
}

fn read_keystore() -> Result<Option<Credentials>, String> {
    let Some(raw) = crate::secrets::get(crate::secrets::HOST)? else {
        return Ok(None);
    };

    // Stored as `username\npassword`: the keystore holds one secret per
    // entry, and the username is not sensitive enough to need a second.
    let (username, password) = raw
        .split_once('\n')
        .ok_or_else(|| "the stored credential was not in the expected form".to_string())?;
    Ok(Some(Credentials {
        username: username.to_string(),
        password: password.to_string(),
    }))
}

fn to_keystore(credentials: &Credentials) -> Result<(), String> {
    crate::secrets::set(
        crate::secrets::HOST,
        &format!("{}\n{}", credentials.username, credentials.password),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sunshine() -> Credentials {
        Credentials {
            username: "sunshine".into(),
            password: "hunter2".into(),
        }
    }

    /// A store whose keystore reads are counted and whose answer is settable,
    /// and whose writes go nowhere. Both counters are per-store rather than
    /// global so these can run in parallel.
    ///
    /// The stubbed writer is not ceremony. When only the reader was stubbed,
    /// a test called straight through to the real Keychain and deleted the
    /// developer's own Sunshine password every time the suite ran —
    /// indistinguishable, from the app, from a password that had never
    /// saved in the first place.
    fn store_answering(
        answer: Result<Option<Credentials>, String>,
    ) -> (CredentialStore, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let store = CredentialStore {
            reader: Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                answer.clone()
            }),
            writer: Arc::new(|_| Ok(())),
            ..Default::default()
        };
        (store, reads)
    }

    fn store() -> (CredentialStore, Arc<AtomicUsize>) {
        store_answering(Ok(Some(sunshine())))
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
    async fn peeking_never_reads_the_keystore() {
        // This is what snapshots use, and a snapshot is on the path to
        // drawing the window. Reading here is what left Dusk opening to an
        // empty grid behind a Keychain prompt.
        let (store, reads) = store();
        for _ in 0..25 {
            assert_eq!(store.peek().await, Known::Unknown);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn peeking_reports_the_answer_once_the_read_has_landed() {
        let (store, _) = store();
        assert_eq!(store.peek().await, Known::Unknown);
        store.prime().await;
        assert_eq!(store.peek().await, Known::SignedIn);
    }

    #[tokio::test]
    async fn an_empty_keystore_is_a_definitive_signed_out() {
        let (store, reads) = store_answering(Ok(None));
        store.prime().await;
        assert_eq!(store.peek().await, Known::SignedOut);
        // Settled, so nothing goes back to the keystore for it.
        store.load().await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_read_is_not_mistaken_for_a_missing_password() {
        // A dismissed Keychain prompt. Caching this as "signed out" is what
        // made a stored password look like it had never been saved.
        let (store, reads) = store_answering(Err("user cancelled".into()));
        store.prime().await;
        assert_eq!(
            store.peek().await,
            Known::Unknown,
            "a keystore that would not answer is not an answer"
        );
        store.load().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "the next thing that needs the password should ask again"
        );
    }

    #[tokio::test]
    async fn a_keystore_that_will_not_answer_is_an_error_not_a_sign_out() {
        // Turning on hosting sets a fresh password when none is stored.
        // A locked keyring must not read as "none is stored", or a host
        // that was working gets its password rotated and restarted.
        let (store, _) = store_answering(Err("keyring is locked".into()));
        let err = store.try_load().await.expect_err("not an answer");
        assert!(err.contains("locked"));
        assert_eq!(store.peek().await, Known::Unknown);

        let (store, _) = store_answering(Ok(None));
        assert_eq!(
            store.try_load().await,
            Ok(None),
            "an empty keystore is a definite no"
        );
        assert_eq!(store.peek().await, Known::SignedOut);
    }

    #[tokio::test]
    async fn a_session_fallback_is_readable_but_not_persistent() {
        // A keystore that cannot be written: the credential still works for
        // the run, but must not claim to have been saved.
        let (store, _) = store();
        let mut cached = store.cached.write().await;
        cached.persistent = false;
        cached.credentials = Some(sunshine());
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

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

use tokio::sync::RwLock;

const SERVICE: &str = "dev.dusk.app";
const ACCOUNT: &str = "sunshine-local-api";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

pub struct CredentialStore {
    /// Populated when the keystore is unavailable, so the app still works
    /// for the session. Never written to disk.
    fallback: RwLock<Option<Credentials>>,
    keystore_available: RwLock<bool>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self {
            fallback: RwLock::new(None),
            keystore_available: RwLock::new(true),
        }
    }
}

impl CredentialStore {
    pub async fn load(&self) -> Option<Credentials> {
        if let Some(found) = self.from_keystore() {
            return Some(found);
        }
        self.fallback.read().await.clone()
    }

    pub async fn save(&self, credentials: Credentials) -> Result<(), String> {
        match self.to_keystore(&credentials) {
            Ok(()) => {
                *self.keystore_available.write().await = true;
                // Drop any session copy: the keystore is now the one truth.
                *self.fallback.write().await = None;
                Ok(())
            }
            Err(err) => {
                eprintln!("dusk: keystore unavailable ({err}); holding credentials for this run only");
                *self.keystore_available.write().await = false;
                *self.fallback.write().await = Some(credentials);
                Ok(())
            }
        }
    }

    pub async fn forget(&self) {
        *self.fallback.write().await = None;
        if let Ok(entry) = keyring::Entry::new(SERVICE, ACCOUNT) {
            // A missing entry is the desired end state, not a failure.
            let _ = entry.delete_credential();
        }
    }

    /// False when the password is only held in memory, so the UI can say
    /// that it will be asked for again next launch.
    pub async fn is_persistent(&self) -> bool {
        *self.keystore_available.read().await && self.fallback.read().await.is_none()
    }

    fn from_keystore(&self) -> Option<Credentials> {
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

    fn to_keystore(&self, credentials: &Credentials) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())?;
        entry
            .set_password(&format!("{}\n{}", credentials.username, credentials.password))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_session_fallback_is_readable_but_not_persistent() {
        let store = CredentialStore::default();
        *store.keystore_available.write().await = false;
        *store.fallback.write().await = Some(Credentials {
            username: "sunshine".into(),
            password: "hunter2".into(),
        });

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

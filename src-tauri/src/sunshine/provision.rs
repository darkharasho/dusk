//! Giving Sunshine a sign-in nobody has to know.
//!
//! Sunshine's local API wants a username and password, and until now Dusk
//! asked the person for the ones they set in Sunshine's own web page — which
//! meant introducing them to Sunshine, the thing Dusk exists to hide. Instead
//! Dusk makes up a credential, hands it to Sunshine with `sunshine --creds`,
//! keeps it in the keystore, and never mentions it again.
//!
//! `--creds` writes Sunshine's credentials file and exits without touching
//! the running service, which only reads that file at startup. So setting a
//! credential is always followed by a restart; that part belongs to the host
//! backend, because restarting a service is platform-shaped and the rest of
//! this is not.
//!
//! It replaces whatever password Sunshine had. That is the point when Dusk
//! installed Sunshine itself, and a deliberate trade when it did not: the
//! person stops being able to sign in to Sunshine's web page, and stops
//! needing to.

use super::{CredentialStore, Credentials, SunshineApi};
use crate::host::HostBackend;

/// Not a secret, and not chosen to be guessable either: it is what someone
/// would see if they ever looked in Sunshine's credentials file and wondered
/// who set it.
pub const USERNAME: &str = "dusk";

/// Long enough that guessing is not a strategy, and alphanumeric so it
/// survives every shell and command line it passes through unquoted — a
/// Windows elevation hop among them — without any escaping to get wrong.
const PASSWORD_LEN: usize = 32;
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// A fresh credential, or an error when the system will not supply
/// randomness. A predictable password guarding the host's whole
/// configuration is worse than no password, so there is no fallback.
pub fn generate() -> Result<Credentials, String> {
    let password = password_from(|buf| {
        getrandom::getrandom(buf)
            .map_err(|e| format!("the system would not supply randomness for a password: {e}"))
    })?;
    Ok(Credentials {
        username: USERNAME.to_string(),
        password,
    })
}

/// Map random bytes onto the alphabet without modulo bias: bytes at or past
/// the largest multiple of the alphabet length are dropped and more are
/// drawn. Takes the source of bytes so the mapping is testable.
fn password_from(mut fill: impl FnMut(&mut [u8]) -> Result<(), String>) -> Result<String, String> {
    let limit = 256 / ALPHABET.len() * ALPHABET.len();
    let mut out = String::with_capacity(PASSWORD_LEN);
    let mut buf = [0u8; PASSWORD_LEN];
    while out.len() < PASSWORD_LEN {
        fill(&mut buf)?;
        for &b in buf.iter().filter(|&&b| (b as usize) < limit) {
            if out.len() == PASSWORD_LEN {
                break;
            }
            out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
        }
    }
    Ok(out)
}

/// The arguments that set a credential, after the Sunshine binary.
pub fn creds_args(credentials: &Credentials) -> [&str; 3] {
    ["--creds", &credentials.username, &credentials.password]
}

/// Give this machine's host a sign-in of Dusk's own, and keep it.
///
/// Takes the two things it actually needs rather than the whole application
/// state, so the headless `dusk setup` runs the same code the window does.
/// A second implementation of this is the last thing anyone wants: it is the
/// step that decides whether a host is configurable at all.
pub async fn run(host: &dyn HostBackend, store: &CredentialStore) -> Result<(), String> {
    let credentials = generate()?;
    host.set_credentials(&credentials)
        .await
        .map_err(|e| e.to_string())?;

    // Kept before it is checked, not after. From here this is Sunshine's
    // password whether or not the check below passes, and losing it would
    // leave a host nobody can configure until this runs again.
    store.save(credentials.clone()).await?;

    let api = SunshineApi::new(super::api::DEFAULT_PORT).map_err(|e| e.to_string())?;
    wait_until_accepted(&api, &credentials).await?;

    // This is the moment Dusk takes over presenting this host, so it is also
    // the moment Sunshine's own tray icon should go. Doing it here and not at
    // every launch matters: applying it needs a restart, and restarting
    // Sunshine on a schedule Dusk chooses would cut off whatever is
    // streaming. Failing is not worth failing over — the sign-in works
    // either way and a second icon is a blemish, not a breakage.
    if let Err(err) = hide_tray(&api, &credentials).await {
        eprintln!("dusk: could not hide Sunshine's tray icon: {err}");
    }
    Ok(())
}

/// Sunshine was just restarted to read the new credential, and takes a few
/// seconds to listen again. Until it does, "unreachable" means "not yet".
async fn wait_until_accepted(api: &SunshineApi, credentials: &Credentials) -> Result<(), String> {
    const ATTEMPTS: u32 = 20;
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match api.verify(credentials).await {
            Ok(_) => return Ok(()),
            // Answering and refusing is a real failure, not a slow start.
            Err(super::ApiError::Unauthorized) => {
                return Err("Hosting restarted but did not accept the sign-in Dusk set.".into())
            }
            Err(err) => last = err.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(format!(
        "Hosting did not come back after restarting: {last}"
    ))
}

/// Turn Sunshine's tray icon off and restart it if that changed anything.
async fn hide_tray(api: &SunshineApi, credentials: &Credentials) -> Result<(), String> {
    let changed = api
        .set_tray(credentials, false)
        .await
        .map_err(|e| e.to_string())?;
    if changed {
        api.restart(credentials).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_password_is_long_and_needs_no_quoting() {
        let c = generate().unwrap();
        assert_eq!(c.username, USERNAME);
        assert_eq!(c.password.len(), PASSWORD_LEN);
        assert!(c.password.bytes().all(|b| b.is_ascii_alphanumeric()));
    }

    #[test]
    fn two_credentials_are_not_the_same() {
        assert_ne!(generate().unwrap().password, generate().unwrap().password);
    }

    #[test]
    fn biased_bytes_are_skipped_rather_than_folded_onto_the_alphabet() {
        // 248..=255 would wrap onto the first eight letters and make them
        // likelier. They are dropped and more are drawn instead.
        let mut draws = 0;
        let p = password_from(|buf| {
            draws += 1;
            buf.fill(if draws == 1 { 255 } else { 0 });
            Ok(())
        })
        .unwrap();
        assert_eq!(p, "A".repeat(PASSWORD_LEN));
        assert_eq!(draws, 2);
    }

    #[test]
    fn no_randomness_means_no_password() {
        assert!(password_from(|_| Err("none".into())).is_err());
    }

    #[test]
    fn the_creds_flag_takes_username_then_password() {
        let c = Credentials {
            username: "dusk".into(),
            password: "pw".into(),
        };
        assert_eq!(creds_args(&c), ["--creds", "dusk", "pw"]);
    }
}

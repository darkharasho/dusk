//! Updating a Dusk that was installed from a `.deb`.
//!
//! The updater plugin cannot do this one. It knows how to replace an
//! AppImage, an `.app`, an MSI and an NSIS install, and a `.deb` is none of
//! those: the files belong to dpkg, and writing over them behind its back
//! would leave the package database describing a version that is no longer
//! there. So this was left to "download it and run a command yourself",
//! which is the worst update story of the five and the one on the machine
//! most likely to be a host in a cupboard.
//!
//! What happens instead: Dusk reads the same manifest the plugin reads,
//! fetches the `.deb` listed in it, **checks the same signature** the plugin
//! would check, and hands it to the package manager with one
//! administrator prompt. dpkg stays the owner of the files throughout —
//! this is `apt install ./dusk.deb`, not a copy over the top.
//!
//! Three things are deliberate:
//!
//! - **The key and the endpoint come from `tauri.conf.json`**, through
//!   `build.rs`, rather than being written down again here. A second copy
//!   that drifted would reject every update, on the one install format
//!   nobody develops on.
//! - **The signature is checked before anything is elevated.** The package
//!   is handed to a root process, so "it came from the right place" has to
//!   be settled first, by the same minisign key that guards every other
//!   platform's update.
//! - **It is never silent.** The download happens on its own, like
//!   everywhere else, and the install waits for the person at the machine:
//!   an administrator prompt out of nowhere is indistinguishable from
//!   something nasty.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::host::service;

/// Set by `build.rs` from `tauri.conf.json`. Empty if this build has no
/// updater configured, which is what a fork without the signing key gets.
const PUBKEY: &str = match option_env!("DUSK_UPDATER_PUBKEY") {
    Some(key) => key,
    None => "",
};
const ENDPOINT: &str = match option_env!("DUSK_UPDATER_ENDPOINT") {
    Some(url) => url,
    None => "",
};

/// Long enough for a slow link to fetch a hundred megabytes.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// What the manifest says about the `.deb`, under a key of its own.
///
/// Beside `platforms` rather than in it: the updater plugin reads
/// `platforms` and would try to install this as though it were an AppImage.
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    deb: Option<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    url: String,
    signature: String,
}

/// An update that is there to be had.
#[derive(Debug, Clone, Serialize)]
pub struct Available {
    pub version: String,
}

/// The update found, and the file once it has been fetched and checked.
#[derive(Debug)]
struct Pending {
    version: String,
    asset: Asset,
    downloaded: Option<PathBuf>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Whether this copy of Dusk is a `.deb` install that can update itself.
///
/// A release build on Linux that is not an AppImage. Not every such build is
/// really a `.deb` — someone could have built from source — but the install
/// is `apt install` on a file, which refuses politely if the package is not
/// wanted, and the alternative is reading dpkg's database to ask a question
/// its answer barely changes.
pub fn is_deb_install() -> bool {
    cfg!(target_os = "linux")
        && !cfg!(debug_assertions)
        && std::env::var_os("APPIMAGE").is_none()
        && !PUBKEY.is_empty()
        && !ENDPOINT.is_empty()
}

/// Is `remote` a later version than `local`?
///
/// Dotted numbers, compared as numbers. `0.3.9` is older than `0.3.27`,
/// which a string comparison gets backwards — the whole reason this is not
/// just `remote != local`. Anything unparseable is treated as not newer: a
/// manifest Dusk cannot read is not grounds for installing something.
fn is_newer(remote: &str, local: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect()
    };
    match (parts(remote), parts(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Check the manifest for a newer version.
pub async fn check(http: &reqwest::Client) -> Result<Option<Available>, String> {
    let manifest: Manifest = http
        .get(ENDPOINT)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("could not reach the update server: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the update manifest could not be read: {e}"))?;

    if !is_newer(&manifest.version, env!("CARGO_PKG_VERSION")) {
        *PENDING.lock().unwrap() = None;
        return Ok(None);
    }
    let Some(asset) = manifest.deb else {
        // A release from before this existed, or one whose Linux build did
        // not finish. Not an error: there is simply nothing to install.
        return Ok(None);
    };

    let version = manifest.version.clone();
    *PENDING.lock().unwrap() = Some(Pending {
        version: version.clone(),
        asset,
        downloaded: None,
    });
    Ok(Some(Available { version }))
}

/// Fetch the package and check its signature. Returns once it is ready to
/// install.
pub async fn download(http: &reqwest::Client) -> Result<(), String> {
    let (version, asset) = {
        let pending = PENDING.lock().unwrap();
        let pending = pending.as_ref().ok_or("there is no update to download")?;
        (pending.version.clone(), pending.asset.clone())
    };

    let bytes = http
        .get(&asset.url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("the update could not be downloaded: {e}"))?
        .error_for_status()
        .map_err(|e| format!("the update could not be downloaded: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("the update could not be downloaded: {e}"))?;

    verify(&bytes, &asset.signature)?;

    // Named for the version so two attempts cannot collide, and written
    // only after the signature holds: nothing unchecked is ever left on
    // disk where a person might run it by hand.
    let path = std::env::temp_dir().join(format!("dusk-update-{version}.deb"));
    std::fs::write(&path, &bytes).map_err(|e| format!("the update could not be saved: {e}"))?;

    if let Some(pending) = PENDING.lock().unwrap().as_mut() {
        pending.downloaded = Some(path);
    }
    Ok(())
}

/// Check a download against the key this build was given.
///
/// Both the key and the signature are base64 of the minisign files, which is
/// how `tauri signer sign` writes them and how the plugin reads them.
fn verify(bytes: &[u8], signature: &str) -> Result<(), String> {
    verify_with(PUBKEY, bytes, signature)
}

/// The key is a parameter so that accepting a *good* signature can be
/// tested. Proving this rejects a bad one is the easy half and the less
/// useful: a verifier that refuses everything also passes that, and would
/// leave every Debian install unable to update while looking careful.
fn verify_with(pubkey: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    use base64::Engine;
    let decode = |what: &str, value: &str| -> Result<String, String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .map_err(|e| format!("the update's {what} was not readable: {e}"))?;
        String::from_utf8(raw).map_err(|e| format!("the update's {what} was not readable: {e}"))
    };

    let key = minisign_verify::PublicKey::decode(&decode("key", pubkey)?)
        .map_err(|e| format!("this build's update key is not usable: {e}"))?;
    let signature = minisign_verify::Signature::decode(&decode("signature", signature)?)
        .map_err(|e| format!("the update's signature was not readable: {e}"))?;

    key.verify(bytes, &signature, false)
        .map_err(|_| "the update was not signed by Dusk and has been discarded".to_string())
}

/// Hand the package to the package manager, with one administrator prompt.
///
/// `apt-get install` on a local file rather than `dpkg -i`, so that any
/// dependency the new version needs is fetched instead of leaving the
/// package half-configured. `--only-upgrade` is deliberately *not* used:
/// this is the same package, and apt treats a file argument as the thing to
/// install at the version in it.
pub async fn install() -> Result<(), String> {
    let path = {
        let pending = PENDING.lock().unwrap();
        pending
            .as_ref()
            .and_then(|p| p.downloaded.clone())
            .ok_or("there is no downloaded update to install")?
    };
    let path = path
        .to_str()
        .ok_or("the update's path is not usable")?
        .to_string();
    if !Path::new(&path).is_file() {
        return Err("the downloaded update is no longer there".into());
    }

    let run = service::run_for(
        "pkexec",
        &[
            "apt-get",
            "install",
            "-y",
            "--allow-downgrades",
            "--reinstall",
            &path,
        ],
        service::ELEVATED_TIMEOUT,
    )
    .await
    .map_err(|e| format!("the update could not be installed: {e}"))?;

    if !run.ok() {
        // 126 is polkit's "not authorised", which here means the prompt was
        // dismissed. Said plainly rather than as a package manager error.
        if run.status == Some(126) || run.status == Some(127) {
            return Err("The update needs permission to install, and that was not given.".into());
        }
        return Err(format!(
            "the package manager refused the update: {}",
            run.message()
        ));
    }
    let _ = std::fs::remove_file(&path);
    *PENDING.lock().unwrap() = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers_not_as_text() {
        // The one that matters: by text, "0.3.9" sorts after "0.3.27" and
        // every machine on 0.3.9 would sit there believing it was current.
        assert!(is_newer("0.3.27", "0.3.9"));
        assert!(!is_newer("0.3.9", "0.3.27"));

        assert!(is_newer("0.4.0", "0.3.27"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.27", "0.3.27"));
        // Downgrades are not updates.
        assert!(!is_newer("0.3.26", "0.3.27"));
        // A leading v, as a tag would carry it.
        assert!(is_newer("v0.3.28", "0.3.27"));
    }

    #[test]
    fn an_unreadable_version_is_not_grounds_for_installing_anything() {
        assert!(!is_newer("nightly", "0.3.27"));
        assert!(!is_newer("0.3.27-rc1", "0.3.27"));
        assert!(!is_newer("", "0.3.27"));
    }

    #[test]
    fn a_manifest_without_a_deb_is_read_rather_than_refused() {
        // Every release before this feature existed, and any whose Linux
        // build did not finish. The rest of the manifest still has to parse,
        // or an AppImage's update would break alongside it.
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.3.27","notes":"x","platforms":{"linux-x86_64":{"url":"u","signature":"s"}}}"#,
        )
        .expect("parses");
        assert_eq!(manifest.version, "0.3.27");
        assert!(manifest.deb.is_none());
    }

    #[test]
    fn the_deb_is_read_from_its_own_key() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.3.28","platforms":{},"deb":{"url":"https://example/d.deb","signature":"sig"}}"#,
        )
        .expect("parses");
        let deb = manifest.deb.expect("a deb entry");
        assert_eq!(deb.url, "https://example/d.deb");
        assert_eq!(deb.signature, "sig");
    }

    #[test]
    fn the_build_picked_up_the_key_the_plugin_uses() {
        // `build.rs` reads these out of tauri.conf.json. If that quietly
        // stopped working, `is_deb_install` would answer false and every
        // Debian install would go back to updating by hand — with nothing
        // to say why. Conditional on the config actually having them, so a
        // fork with no signing key of its own still builds and tests clean.
        let raw = std::fs::read_to_string("tauri.conf.json").expect("run from src-tauri");
        let config: serde_json::Value = serde_json::from_str(&raw).expect("parses");
        let updater = &config["plugins"]["updater"];

        if let Some(key) = updater["pubkey"].as_str() {
            assert_eq!(PUBKEY, key, "the baked key is not the configured one");
        }
        if let Some(endpoint) = updater["endpoints"][0].as_str() {
            assert_eq!(
                ENDPOINT, endpoint,
                "the baked endpoint is not the configured one"
            );
        }
    }

    /// A throwaway key and a real `tauri signer sign` signature over
    /// [`SIGNED`], made with it. Not Dusk's key — nothing here is trusted by
    /// anything — but the same format, so this exercises exactly what a
    /// release artifact's signature goes through.
    const TEST_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEJBRkZDOENCNTc1MDIxQTgKUldTb0lWQlh5OGovdXZIRmZrV1UxYXQvNTQvejJ6SjJEU1E2U0V2M1pzcGwwMjNRTU1VYkZ5REcK";
    const TEST_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVTb0lWQlh5OGovdWwrRTd2bUZqYU9HNUxXRGkzei92bGdHU0ZXbTZqcVJUclI4SERGWE10clYyRW56RUhwOHI4bjRHdmVSSi9ha2NGL0UvN3RlZUEyeXcwWWl5ZkF0NkFRPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyNjEwNjc2CWZpbGU6Zml4dHVyZS5iaW4KamFEdFJRSjB5Skl2SEhhenFHRGE2ZmoxblFyWURjeHRYUkl2UHRHQ1pLNHl1SmZPMERvVC94K2dFNVptZHh5TklCZU1RZTl6VTVwVUVrcm1HTnNSQmc9PQo=";
    const SIGNED: &[u8] = b"a dusk package\n";

    /// Check a real release artifact against the key this build carries.
    ///
    /// Ignored because it needs files that are not in the repository. Run it
    /// on a release's `.deb` and the `.deb.sig` beside it to prove the whole
    /// chain — CI's signing step, the key in `tauri.conf.json`, and this
    /// verifier — agree. Everything else here uses a throwaway key, so
    /// without this nothing confirms the *published* artifact is one Dusk
    /// would accept, and the failure would be silent and total.
    ///
    ///     DUSK_TEST_DEB=/path/Dusk_x_amd64.deb \
    ///     cargo test --lib the_released_deb -- --ignored --nocapture
    #[test]
    #[ignore]
    fn the_released_deb_verifies_against_this_builds_key() {
        let path = std::env::var("DUSK_TEST_DEB").expect("set DUSK_TEST_DEB to a released .deb");
        let bytes = std::fs::read(&path).expect("the .deb is readable");
        let signature =
            std::fs::read_to_string(format!("{path}.sig")).expect("a .sig beside the .deb");
        verify(&bytes, &signature).expect("the released .deb verifies");
        println!("verified {} ({} bytes)", path, bytes.len());
    }

    #[test]
    fn a_genuine_signature_is_accepted() {
        // The half that matters. A verifier that only ever refuses passes
        // every "rejects a bad signature" test there is, and would leave
        // every Debian install unable to update while looking careful.
        verify_with(TEST_PUBKEY, SIGNED, TEST_SIG).expect("a real signature over these bytes");
    }

    #[test]
    fn an_update_that_does_not_match_its_signature_is_refused() {
        // The same good signature, over bytes it was not made for: what a
        // package tampered with in flight looks like.
        assert!(verify_with(TEST_PUBKEY, b"a different package\n", TEST_SIG).is_err());

        // A real signature over the right bytes, checked against a
        // different key: what an update signed by someone else looks like.
        // A second generated keypair, not the first one's comment edited —
        // minisign keeps the key material on the second line, so changing
        // the comment leaves the same key and verifies perfectly well.
        let other = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDBDOUM5MDI1NjRGQkQ1NkYKUldSdjFmdGtKWkNjREk4SnpJNnRsTTBjZXhGd0ZMR0s5c3dQOFRRTWUwOEI1Zitab3ZHT2MxcGgK";
        assert!(verify_with(other, SIGNED, TEST_SIG).is_err());

        // Not a signature at all.
        assert!(verify_with(TEST_PUBKEY, SIGNED, "bm90IGEgc2lnbmF0dXJl").is_err());

        // No key baked in: nothing to check against must not read as "fine".
        assert!(verify_with("", SIGNED, TEST_SIG).is_err());
    }
}

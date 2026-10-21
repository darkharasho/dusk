//! Addresses that only your own machines can read.
//!
//! The account's machine list lives in a secret gist, and a secret gist is
//! unlisted rather than private: anyone holding the URL can read it, and that
//! URL sits in every machine's keystore. Names and public keys are
//! unremarkable there. A list of where your machines answer is a map of your
//! network, and it is the first thing Dusk publishes *about* that network
//! rather than about the machines on it. So it is sealed — see
//! `docs/addresses.md` for the argument.
//!
//! # Why a second key
//!
//! Each machine already has an Ed25519 keypair, and Ed25519 signs; it does
//! not encrypt. Converting it to X25519 is possible but needs the private
//! scalar, which `ring` does not expose, and reusing one key for two purposes
//! is the kind of thing that is fine until it is not. So each machine grows a
//! second, X25519 keypair, and publishes its public half beside the signing
//! one.
//!
//! `ring` cannot hold the receiving end of this: its X25519 is deliberately
//! ephemeral-only, consumed by one agreement, and a recipient's key has to
//! outlive the message. Hence `x25519-dalek`, which is the narrow, ordinary
//! choice for exactly this.
//!
//! # The shape
//!
//! Sealing is ephemeral-static ECDH, once per recipient:
//!
//! 1. the sender makes a throwaway X25519 keypair and publishes its public half
//! 2. for each recipient, it agrees with that recipient's published key
//! 3. HKDF over the shared secret gives a key, and ChaCha20-Poly1305 seals
//!    the payload under it
//!
//! A recipient agrees its own static key with the published ephemeral one,
//! derives the same key, and opens the box addressed to it. Anyone else gets
//! a gist full of ciphertext.
//!
//! The payload is a few dozen bytes, so it is sealed once per recipient
//! rather than wrapping a content key. With a handful of machines that is
//! cheaper to do and much cheaper to reason about.

use std::collections::BTreeMap;

use ring::{aead, hkdf};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::install::download::hex;

/// What the sender publishes: one throwaway public key, and one box per
/// machine that was able to receive it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// The sender's throwaway public key, hex.
    pub ephemeral: String,
    /// Machine id to sealed payload, hex, nonce first.
    pub to: BTreeMap<String, String>,
}

/// This machine's long-lived key for receiving sealed messages.
#[derive(Clone)]
pub struct BoxKey {
    secret: StaticSecret,
}

impl std::fmt::Debug for BoxKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret, by construction rather than by discipline.
        f.debug_struct("BoxKey")
            .field("public", &self.public_hex())
            .finish()
    }
}

impl BoxKey {
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| format!("the system would not supply randomness: {e}"))?;
        Ok(Self {
            secret: StaticSecret::from(bytes),
        })
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes: [u8; 32] = unhex(s)?.try_into().ok()?;
        Some(Self {
            secret: StaticSecret::from(bytes),
        })
    }

    pub fn to_hex(&self) -> String {
        hex(&self.secret.to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex(PublicKey::from(&self.secret).as_bytes())
    }
}

/// Seal `payload` so that each of `recipients` — and nobody else — can read it.
///
/// A recipient whose published key is unusable is skipped rather than failing
/// the whole send: one machine with a malformed entry must not stop the rest
/// of the account from learning where this one is.
pub fn seal(payload: &[u8], recipients: &[(String, String)]) -> Result<Envelope, String> {
    let ephemeral = BoxKey::generate()?;
    let mut to = BTreeMap::new();

    for (id, public_hex) in recipients {
        let Some(public) = public_key(public_hex) else {
            continue;
        };
        let shared = ephemeral.secret.diffie_hellman(&public);
        let sealed = encrypt(shared.as_bytes(), payload)?;
        to.insert(id.clone(), sealed);
    }

    Ok(Envelope {
        ephemeral: ephemeral.public_hex(),
        to,
    })
}

/// Open the box addressed to `me`, if there is one and it is ours to open.
///
/// `None` for every ordinary reason — no box for this machine, a sender that
/// predates sealing, a payload that does not authenticate — because all of
/// them mean the same thing to the caller: no addresses from this machine.
pub fn open(envelope: &Envelope, me: &BoxKey, my_id: &str) -> Option<Vec<u8>> {
    let sealed = envelope.to.get(my_id)?;
    let public = public_key(&envelope.ephemeral)?;
    let shared = me.secret.diffie_hellman(&public);
    decrypt(shared.as_bytes(), sealed)
}

fn public_key(hex: &str) -> Option<PublicKey> {
    let bytes: [u8; 32] = unhex(hex)?.try_into().ok()?;
    Some(PublicKey::from(bytes))
}

/// A shared secret is not a key: it is a curve point with structure, and
/// using it directly is the classic way to weaken this. HKDF with a context
/// string of our own gives a key that is only good for this purpose.
fn derive(shared: &[u8]) -> Result<aead::LessSafeKey, String> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"dusk-addresses-v1");
    let prk = salt.extract(shared);
    let okm = prk
        .expand(&[b"addresses"], &aead::CHACHA20_POLY1305)
        .map_err(|_| "could not derive a key".to_string())?;
    let key = aead::UnboundKey::from(okm);
    Ok(aead::LessSafeKey::new(key))
}

fn encrypt(shared: &[u8], payload: &[u8]) -> Result<String, String> {
    let key = derive(shared)?;
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce)
        .map_err(|e| format!("the system would not supply randomness: {e}"))?;

    let mut buffer = payload.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut buffer,
    )
    .map_err(|_| "could not seal the addresses".to_string())?;

    let mut out = nonce.to_vec();
    out.extend_from_slice(&buffer);
    Ok(hex(&out))
}

fn decrypt(shared: &[u8], sealed: &str) -> Option<Vec<u8>> {
    let raw = unhex(sealed)?;
    if raw.len() < 12 {
        return None;
    }
    let (nonce, body) = raw.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().ok()?;

    let key = derive(shared).ok()?;
    let mut buffer = body.to_vec();
    let opened = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut buffer,
        )
        .ok()?;
    Some(opened.to_vec())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> (String, BoxKey) {
        let key = BoxKey::generate().unwrap();
        (key.public_hex(), key)
    }

    #[test]
    fn a_machine_can_read_what_was_sealed_to_it() {
        let (venus_pub, venus) = machine();
        let envelope = seal(b"100.101.8.12", &[("venus".into(), venus_pub)]).unwrap();
        assert_eq!(
            open(&envelope, &venus, "venus").as_deref(),
            Some(&b"100.101.8.12"[..])
        );
    }

    #[test]
    fn another_machine_cannot() {
        // The whole point: the gist is readable by anyone holding its URL,
        // so a box not addressed to you must stay shut even with it in hand.
        let (venus_pub, _venus) = machine();
        let (_, stranger) = machine();
        let envelope = seal(b"100.101.8.12", &[("venus".into(), venus_pub)]).unwrap();
        assert_eq!(open(&envelope, &stranger, "venus"), None);
    }

    #[test]
    fn every_recipient_gets_its_own_box() {
        let (a_pub, a) = machine();
        let (b_pub, b) = machine();
        let envelope = seal(b"addresses", &[("a".into(), a_pub), ("b".into(), b_pub)]).unwrap();
        assert_eq!(envelope.to.len(), 2);
        assert_eq!(open(&envelope, &a, "a").as_deref(), Some(&b"addresses"[..]));
        assert_eq!(open(&envelope, &b, "b").as_deref(), Some(&b"addresses"[..]));
        // Addressed by id, so one machine cannot open another's box even on
        // the same account.
        assert_eq!(open(&envelope, &a, "b"), None);
    }

    #[test]
    fn a_tampered_box_does_not_open() {
        // ChaCha20-Poly1305 authenticates, so a gist someone edited is
        // refused rather than believed.
        let (venus_pub, venus) = machine();
        let mut envelope = seal(b"100.101.8.12", &[("venus".into(), venus_pub)]).unwrap();
        let sealed = envelope.to.get_mut("venus").unwrap();
        let flipped = match sealed.pop().unwrap() {
            '0' => '1',
            _ => '0',
        };
        sealed.push(flipped);
        assert_eq!(open(&envelope, &venus, "venus"), None);
    }

    #[test]
    fn a_recipient_with_an_unusable_key_is_skipped_not_fatal() {
        // One machine with a malformed entry must not stop the rest of the
        // account from learning where this one is.
        let (good_pub, good) = machine();
        let envelope = seal(
            b"addresses",
            &[
                ("broken".into(), "nothex".into()),
                ("good".into(), good_pub),
            ],
        )
        .unwrap();
        assert!(!envelope.to.contains_key("broken"));
        assert_eq!(
            open(&envelope, &good, "good").as_deref(),
            Some(&b"addresses"[..])
        );
    }

    #[test]
    fn a_key_survives_the_round_trip_through_the_keystore() {
        let key = BoxKey::generate().unwrap();
        let same = BoxKey::from_hex(&key.to_hex()).expect("its own hex");
        assert_eq!(same.public_hex(), key.public_hex());
        assert!(BoxKey::from_hex("not hex").is_none());
        assert!(BoxKey::from_hex("00ff").is_none(), "wrong length");
    }

    #[test]
    fn two_sends_of_the_same_thing_do_not_look_alike() {
        // A fresh ephemeral key and nonce each time, so an observer cannot
        // tell that a machine's addresses are unchanged.
        let (pubkey, _key) = machine();
        let one = seal(b"same", &[("m".into(), pubkey.clone())]).unwrap();
        let two = seal(b"same", &[("m".into(), pubkey)]).unwrap();
        assert_ne!(one.ephemeral, two.ephemeral);
        assert_ne!(one.to.get("m"), two.to.get("m"));
    }
}

//! Each machine's own signing key.
//!
//! A machine signed in to an account proves a pairing request is its own by
//! signing it. The private half never leaves the machine's keystore; the
//! public half is published to the account's machine list, which is where
//! another machine looks it up.

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

use sha2::{Digest, Sha256};

use crate::install::download::hex;
use crate::sunshine::state_file::normalise_pem;

/// What a pairing proof signs. Versioned and prefixed so a signature made
/// for this can never be mistaken for one made for anything else.
pub(crate) fn pair_message(pin: &str) -> Vec<u8> {
    format!("dusk-pair-v1\n{pin}").into_bytes()
}

fn sha256_hex(pem: &str) -> String {
    hex(Sha256::digest(normalise_pem(pem).as_bytes()).as_ref())
}

/// What the client signs to ask a host to trust its certificate. Names the
/// host so the request cannot be replayed to another, and uses its own prefix
/// so it can never pass as a reply.
#[allow(dead_code)] // Task 6 onward builds the pairing messages with it.
pub fn cert_request_message(client_cert_pem: &str, host_uniqueid: &str) -> Vec<u8> {
    format!(
        "dusk-pair-cert-v1\n{host_uniqueid}\n{}",
        sha256_hex(client_cert_pem)
    )
    .into_bytes()
}

/// What the host signs to say it paired that client, and which certificate
/// it will stream under.
#[allow(dead_code)] // Task 6 onward builds the pairing messages with it.
pub fn cert_reply_message(
    client_cert_pem: &str,
    host_uniqueid: &str,
    server_cert_pem: &str,
) -> Vec<u8> {
    format!(
        "dusk-paired-v1\n{host_uniqueid}\n{}\n{}",
        sha256_hex(client_cert_pem),
        sha256_hex(server_cert_pem)
    )
    .into_bytes()
}

#[derive(Clone)]
pub struct MachineKey {
    /// Random, and stable for as long as the machine stays signed in. It
    /// names the machine's entry in the account's list.
    pub id: String,
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for MachineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key itself stays out of logs.
        f.debug_struct("MachineKey").field("id", &self.id).finish()
    }
}

impl MachineKey {
    pub fn generate() -> Result<Self, String> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| "the system would not supply randomness for a key".to_string())?;
        let mut id = [0u8; 8];
        getrandom::getrandom(&mut id).map_err(|e| e.to_string())?;
        Ok(Self {
            id: hex(&id),
            pkcs8: pkcs8.as_ref().to_vec(),
        })
    }

    pub fn from_parts(id: String, pkcs8_hex: &str) -> Result<Self, String> {
        let pkcs8 = unhex(pkcs8_hex).ok_or("the stored key was not in the expected form")?;
        Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| "the stored key is not a valid key")?;
        Ok(Self { id, pkcs8 })
    }

    pub fn pkcs8_hex(&self) -> String {
        hex(&self.pkcs8)
    }

    fn pair(&self) -> Ed25519KeyPair {
        // Checked when the key was made or loaded, so it parses here.
        Ed25519KeyPair::from_pkcs8(&self.pkcs8).expect("a key checked on creation")
    }

    pub fn public_hex(&self) -> String {
        hex(self.pair().public_key().as_ref())
    }

    pub fn sign(&self, message: &[u8]) -> String {
        hex(self.pair().sign(message).as_ref())
    }

    #[allow(dead_code)] // Only tests sign a bare PIN now; callers go through `sign`.
    pub fn sign_pair(&self, pin: &str) -> String {
        self.sign(&pair_message(pin))
    }
}

/// Whether `signature` is `public`'s signature over `message`.
pub fn verify(public_hex: &str, message: &[u8], signature_hex: &str) -> bool {
    let (Some(public), Some(signature)) = (unhex(public_hex), unhex(signature_hex)) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, public)
        .verify(message, &signature)
        .is_ok()
}

/// Whether `signature` is `public`'s signature over a pairing with `pin`.
#[allow(dead_code)] // Only tests verify a bare PIN now; callers go through `verify`.
pub fn verify_pair(public_hex: &str, pin: &str, signature_hex: &str) -> bool {
    verify(public_hex, &pair_message(pin), signature_hex)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_against_its_own_key_and_pin_only() {
        let key = MachineKey::generate().unwrap();
        let sig = key.sign_pair("1234");
        assert!(verify_pair(&key.public_hex(), "1234", &sig));
        assert!(!verify_pair(&key.public_hex(), "1235", &sig));

        let other = MachineKey::generate().unwrap();
        assert!(!verify_pair(&other.public_hex(), "1234", &sig));
    }

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";

    #[test]
    fn a_pin_signature_still_verifies_exactly_as_before() {
        let key = MachineKey::generate().unwrap();
        let sig = key.sign_pair("1234");
        assert!(verify_pair(&key.public_hex(), "1234", &sig));
        assert!(verify(&key.public_hex(), &pair_message("1234"), &sig));
    }

    #[test]
    fn a_request_signature_does_not_verify_for_another_host() {
        let key = MachineKey::generate().unwrap();
        let sig = key.sign(&cert_request_message(CERT, "H1"));
        assert!(verify(
            &key.public_hex(),
            &cert_request_message(CERT, "H1"),
            &sig
        ));
        assert!(!verify(
            &key.public_hex(),
            &cert_request_message(CERT, "H2"),
            &sig
        ));
    }

    #[test]
    fn a_request_signature_cannot_pass_as_a_reply() {
        let key = MachineKey::generate().unwrap();
        let sig = key.sign(&cert_request_message(CERT, "H1"));
        assert!(!verify(
            &key.public_hex(),
            &cert_reply_message(CERT, "H1", CERT),
            &sig
        ));
    }

    #[test]
    fn a_reply_changes_when_either_certificate_does() {
        let other = "-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----";
        let base = cert_reply_message(CERT, "H1", CERT);
        assert_ne!(base, cert_reply_message(CERT, "H1", other));
        assert_ne!(base, cert_reply_message(other, "H1", CERT));
    }

    #[test]
    fn crlf_and_lf_copies_of_a_certificate_sign_the_same() {
        let crlf = CERT.replace('\n', "\r\n") + "\r\n";
        assert_eq!(
            cert_request_message(CERT, "H1"),
            cert_request_message(&crlf, "H1")
        );
        assert_eq!(
            cert_reply_message(CERT, "H1", CERT),
            cert_reply_message(&crlf, "H1", &crlf)
        );
    }

    #[test]
    fn a_key_survives_being_stored() {
        let key = MachineKey::generate().unwrap();
        let back = MachineKey::from_parts(key.id.clone(), &key.pkcs8_hex()).unwrap();
        assert_eq!(back.public_hex(), key.public_hex());
    }

    #[test]
    fn junk_never_verifies() {
        let key = MachineKey::generate().unwrap();
        assert!(!verify_pair(&key.public_hex(), "1234", "zz"));
        assert!(!verify_pair("abc", "1234", &key.sign_pair("1234")));
        assert!(MachineKey::from_parts("x".into(), "00").is_err());
    }
}

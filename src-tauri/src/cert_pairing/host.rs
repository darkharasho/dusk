//! The host's side of pairing by certificate.
//!
//! Another of your machines sends its certificate, signed for this host.
//! Dusk records it as one of your pairings, has the reconciler write it into
//! the host's list of paired machines, and replies with this host's own
//! certificate, so the two can stream with no PIN.

use std::future::Future;
use std::sync::Arc;

use crate::account::keys::{cert_reply_message, cert_request_message};
use crate::account::{Membership, Proof};
use crate::peer::Message;
use crate::state::AppState;
use crate::store::AccountPairing;
use crate::sunshine::state_file::{self, normalise_pem, NamedDevice};

use super::reconcile::{files, ready, Files, Local, Timing};

/// What Dusk's entries in the host's list should be: the pairings to have,
/// and the uuids to take out.
///
/// A list that was never fetched keeps everything: not knowing who is on
/// the account is not the same as nobody being on it, and treating it so
/// would unpair every machine whenever GitHub was unreachable.
pub fn plan(
    pairings: &[AccountPairing],
    membership: &Membership,
) -> (Vec<NamedDevice>, Vec<String>) {
    let (keep, drop): (Vec<_>, Vec<_>) = pairings.iter().partition(|p| match membership {
        Membership::Unknown => true,
        Membership::SignedOut => false,
        Membership::Machines(machines) => machines.contains(&p.machine),
    });
    let add = keep
        .into_iter()
        .map(|p| NamedDevice {
            name: p.name.clone(),
            cert: p.cert.clone(),
            uuid: p.uuid.clone(),
        })
        .collect();
    (add, drop.into_iter().map(|p| p.uuid.clone()).collect())
}

/// Record `machine`'s certificate. A changed certificate gets a fresh uuid,
/// since the host matches entries by uuid and would keep the old one; the
/// uuid it replaced is returned so it can be taken out.
fn upsert(
    pairings: &mut Vec<AccountPairing>,
    machine: &str,
    name: &str,
    cert: &str,
) -> Option<String> {
    let fresh = AccountPairing {
        machine: machine.to_string(),
        name: name.to_string(),
        cert: cert.to_string(),
        uuid: mint_uuid(),
    };
    match pairings.iter_mut().find(|p| p.machine == machine) {
        Some(p) if normalise_pem(&p.cert) == normalise_pem(cert) => None,
        Some(p) => Some(std::mem::replace(p, fresh).uuid),
        None => {
            pairings.push(fresh);
            None
        }
    }
}

/// A random (version 4) uuid, upper case like the host's own.
fn mint_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("the system would not supply randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = crate::install::download::hex(&b).to_uppercase();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// What a reply is built from: this host's id and its certificate.
#[derive(Debug)]
struct Identity {
    uniqueid: String,
    server_cert: String,
}

const UNREADABLE: &str = "This machine couldn't read its own list of paired machines.";

/// Read this host's identity and check the request was signed for it by one
/// of your machines. `vouches` is asked about the exact message expected.
async fn admit<V, F>(files: &Files, cert: &str, vouches: V) -> Result<Identity, String>
where
    V: FnOnce(Vec<u8>) -> F,
    F: Future<Output = bool>,
{
    let state = state_file::read(&files.state).map_err(|_| UNREADABLE.to_string())?;
    let uniqueid = state["root"]["uniqueid"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(UNREADABLE)?
        .to_string();
    let server_cert = std::fs::read_to_string(&files.server_cert)
        .map_err(|_| "This machine couldn't find its own certificate.".to_string())?;
    if !vouches(cert_request_message(cert, &uniqueid)).await {
        return Err("This machine couldn't confirm you're on the same account.".into());
    }
    Ok(Identity {
        uniqueid,
        server_cert,
    })
}

/// Answer a `pair_cert` request: trust the asking machine's certificate if
/// it is one of yours, and reply with this host's own.
pub async fn handle(
    state: &Arc<AppState>,
    address: &str,
    name: String,
    cert: String,
    proof: Proof,
) -> Message {
    match answer(state, address, &name, &cert, &proof).await {
        Ok(reply) => reply,
        Err(reason) => Message::Declined { reason },
    }
}

async fn answer(
    state: &Arc<AppState>,
    address: &str,
    name: &str,
    cert: &str,
    proof: &Proof,
) -> Result<Message, String> {
    let credentials = state
        .credentials
        .load()
        .await
        .ok_or("This machine isn't set up to host yet.")?;
    let files = files(&credentials)
        .await
        .ok_or("This machine isn't set up to host yet.")?;
    let identity = admit(&files, cert, |message| async move {
        state
            .account
            .vouches_for(&state.http, proof, &message)
            .await
    })
    .await?;
    eprintln!("dusk: {name} ({address}) is on this account; pairing by certificate");

    {
        let mut store = state.store.lock().await;
        let retired = upsert(&mut store.account_pairings, &proof.machine, name, cert);
        store.retired_pairings.extend(retired);
        store.save(&state.store_path).map_err(|err| {
            eprintln!("dusk: could not save the pairing with {name}: {err}");
            "This machine couldn't save the pairing.".to_string()
        })?;
    }

    let host = Local {
        http: &state.http,
        credentials: &credentials,
    };
    let membership = state.account.membership().await;
    let at = async { Some(files.state.clone()) };
    let outcome = state
        .reconciler
        .run(&state.store, &state.store_path, &membership, at, &host)
        .await
        .map_err(|err| {
            eprintln!("dusk: could not pair {name} by certificate: {err}");
            "This machine couldn't add yours to its list of paired machines.".to_string()
        })?;
    if outcome.pending {
        // So the loop moves to its shorter poll and restarts the host as
        // soon as the stream ends.
        state.reconciler.nudge();
    }
    let ready = ready(outcome, &host, &Timing::REAL).await;

    let reply = cert_reply_message(cert, &identity.uniqueid, &identity.server_cert);
    let proof = state
        .account
        .sign(&reply)
        .await
        .ok_or("This machine is no longer signed in to your account.")?;
    Ok(Message::Paired {
        server_cert: identity.server_cert,
        uniqueid: identity.uniqueid,
        ready,
        proof,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{on_account, pairing, Dir, CLIENT_CERT, SERVER_CERT};
    use super::*;
    use crate::account::keys::MachineKey;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn an_unknown_membership_keeps_every_pairing() {
        let pairings = [pairing("aa", "D1"), pairing("bb", "D2")];
        let (add, remove) = plan(&pairings, &Membership::Unknown);
        assert_eq!(add.len(), 2);
        assert!(remove.is_empty());
    }

    #[test]
    fn signing_out_removes_every_pairing_dusk_added() {
        let pairings = [pairing("aa", "D1"), pairing("bb", "D2")];
        let (add, remove) = plan(&pairings, &Membership::SignedOut);
        assert!(add.is_empty());
        assert_eq!(remove, ["D1", "D2"]);
    }

    #[test]
    fn a_machine_off_the_account_is_removed_and_the_rest_kept() {
        let pairings = [pairing("aa", "D1"), pairing("bb", "D2")];
        let (add, remove) = plan(&pairings, &on_account(&["aa"]));
        assert_eq!(
            add,
            [NamedDevice {
                name: pairings[0].name.clone(),
                cert: pairings[0].cert.clone(),
                uuid: "D1".into(),
            }]
        );
        assert_eq!(remove, ["D2"]);
    }

    #[test]
    fn a_new_certificate_gets_a_new_uuid_and_retires_the_old() {
        let mut pairings = Vec::new();
        assert_eq!(upsert(&mut pairings, "aa", "Desk", CLIENT_CERT), None);
        let first = pairings[0].uuid.clone();
        assert_eq!(first.len(), 36);
        assert_eq!(first, first.to_uppercase());

        // The same certificate, however its lines end, is the same pairing.
        let crlf = CLIENT_CERT.replace('\n', "\r\n");
        assert_eq!(upsert(&mut pairings, "aa", "Desk", &crlf), None);
        assert_eq!(pairings[0].uuid, first);

        let retired = upsert(&mut pairings, "aa", "Desk", SERVER_CERT);
        assert_eq!(retired, Some(first.clone()));
        assert_eq!(pairings.len(), 1);
        assert_ne!(pairings[0].uuid, first);
        assert_eq!(pairings[0].cert, SERVER_CERT);
    }

    fn signed_by(key: &MachineKey, host: &str) -> Proof {
        Proof {
            account: 42,
            machine: key.id.clone(),
            signature: key.sign(&cert_request_message(CLIENT_CERT, host)),
        }
    }

    #[tokio::test]
    async fn a_request_signed_for_another_host_is_declined() {
        let dir = Dir::new("admit-other-host");
        let key = MachineKey::generate().unwrap();
        let public = key.public_hex();
        let vouch = |proof: Proof| {
            let public = public.clone();
            move |message: Vec<u8>| async move {
                crate::account::keys::verify(&public, &message, &proof.signature)
            }
        };

        let elsewhere = signed_by(&key, "OTHER-HOST");
        let declined = admit(&dir.files(), CLIENT_CERT, vouch(elsewhere))
            .await
            .unwrap_err();
        assert!(!declined.contains("Sunshine"));

        let here = signed_by(&key, "HOST-ID");
        let identity = admit(&dir.files(), CLIENT_CERT, vouch(here)).await.unwrap();
        assert_eq!(identity.uniqueid, "HOST-ID");
        assert_eq!(identity.server_cert, SERVER_CERT);
    }

    #[tokio::test]
    async fn a_state_file_it_cannot_read_declines_rather_than_pairs() {
        let dir = Dir::new("admit-unreadable");
        std::fs::write(dir.state(), "{ not json").unwrap();
        let asked = AtomicUsize::new(0);
        let declined = admit(&dir.files(), CLIENT_CERT, |_| async {
            asked.fetch_add(1, Ordering::SeqCst);
            true
        })
        .await
        .unwrap_err();
        assert!(!declined.contains("Sunshine") && !declined.contains("Moonlight"));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read_to_string(dir.state()).unwrap(), "{ not json");

        // A missing certificate is the same answer, not a pairing.
        let dir = Dir::new("admit-no-cert");
        std::fs::remove_file(dir.0.join("cacert.pem")).unwrap();
        assert!(admit(&dir.files(), CLIENT_CERT, |_| async { true })
            .await
            .is_err());
    }
}

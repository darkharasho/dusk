//! The connecting side of pairing by certificate.
//!
//! When both machines are on your account, this one sends the host its
//! certificate, signed for that host, and gets the host's certificate back.
//! Dusk then has the client record the host as paired, and no PIN is shown.
//! Anything short of a reply that checks out falls back to the PIN, so a
//! host too old for this, or not one of yours, pairs as it always has.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::account::keys::{cert_reply_message, cert_request_message};
use crate::account::Proof;
use crate::model::{Activity, PairingState};
use crate::moonlight::cli::MoonlightError;
use crate::moonlight::Moonlight;
use crate::peer::{CertOutcome, Message};
use crate::serverinfo;
use crate::state::AppState;

/// How often to try again, while the client is open or the host is busy.
const RETRY_EVERY: Duration = Duration::from_secs(5);

/// How long to keep trying while another copy of the client is open.
const CLIENT_OPEN_WAIT: Duration = Duration::from_secs(120);

/// How long a host that is not hosting gets to finish pairing. It restarts
/// once its stream has ended, which it notices within a quarter of a minute
/// and takes about as long again to do.
const IDLE_WAIT: Duration = Duration::from_secs(120);

const SAVE_FAILED: &str = "This machine couldn't save the pairing. Try pairing again.";
const NOT_FINISHED: &str = "That machine hasn't finished pairing yet. Try again in a minute.";

/// Pair with the host behind `device_id` by certificate.
///
/// `Ok(true)` when both halves are done. `Ok(false)` when it was not tried
/// or the host would not, and the PIN should be used instead. `Err` when
/// the host has paired this machine but this machine could not finish its
/// half. `on_waiting_for_host` runs if the host is busy streaming and will
/// finish once it is free.
pub async fn pair(
    state: &Arc<AppState>,
    address: &str,
    device_id: &str,
    on_waiting_for_host: impl FnOnce(),
) -> Result<bool, String> {
    let Some(uniqueid) = crate::registry::unique_id_of(device_id) else {
        return Ok(false);
    };
    let Some((name, http_port, https_port)) = device(state, device_id).await else {
        return Ok(false);
    };
    let Some(client) = state.moonlight.as_ref() else {
        return Ok(false);
    };
    if !client.supports_add_host().await {
        return Ok(false);
    }
    let Some(identity) = crate::moonlight::identity::load(Some(client)) else {
        return Ok(false);
    };
    let Ok(own_cert) = String::from_utf8(identity.cert_pem.clone()) else {
        return Ok(false);
    };
    let Some(proof) = state
        .account
        .sign(&cert_request_message(&own_cert, uniqueid))
        .await
    else {
        return Ok(false);
    };

    let host = crate::peer::host_part(address);
    let request = Message::PairCert {
        name: crate::peer::host_name(),
        cert: own_cert.clone(),
        proof,
    };
    let reply = crate::peer::ask_cert(host, request).await;
    if let CertOutcome::Declined(reason) = &reply {
        eprintln!("dusk: {host} would not pair by certificate ({reason}); using the PIN");
    }
    let vouches = |proof: Proof, message: Vec<u8>| async move {
        state
            .account
            .vouches_for(&state.http, &proof, &message)
            .await
    };
    let Some((server_cert, uniqueid, ready)) =
        accept_reply(&own_cert, uniqueid, reply, vouches).await
    else {
        return Ok(false);
    };

    let at = serverinfo::with_port(address, http_port);
    record(client, &at, &uniqueid, &name, &server_cert).await?;

    if !ready {
        on_waiting_for_host();
        let tls = crate::state::tls_client(&identity).ok_or(NOT_FINISHED)?;
        wait_until_paired(state, &tls, host, http_port, https_port).await?;
    }
    Ok(true)
}

/// Check the host's reply: it has to name the host that was asked, and be
/// signed by one of your machines for this client's certificate and the
/// server certificate it carries. `verify` is asked about the exact message
/// expected. Gives the server certificate, the host's id and whether it is
/// ready, or `None` to fall back to the PIN.
async fn accept_reply<V, F>(
    own_cert: &str,
    device_id: &str,
    reply: CertOutcome,
    verify: V,
) -> Option<(String, String, bool)>
where
    V: FnOnce(Proof, Vec<u8>) -> F,
    F: Future<Output = bool>,
{
    let CertOutcome::Paired {
        server_cert,
        uniqueid,
        ready,
        proof,
    } = reply
    else {
        return None;
    };
    if uniqueid != device_id {
        eprintln!("dusk: a pairing reply named another host; using the PIN");
        return None;
    }
    if !verify(proof, cert_reply_message(own_cert, device_id, &server_cert)).await {
        eprintln!("dusk: a pairing reply did not check out; using the PIN");
        return None;
    }
    Some((server_cert, uniqueid, ready))
}

/// The name and ports the grid has for a device.
async fn device(state: &Arc<AppState>, id: &str) -> Option<(String, u16, u16)> {
    let registry = state.registry.read().await;
    registry
        .devices()
        .into_iter()
        .find(|d| d.id == id)
        .map(|d| (d.name, d.http_port, d.https_port))
}

/// Have the client record the host as paired. A client that is open holds
/// its settings, so this waits a while for it to close.
async fn record(
    client: &Moonlight,
    address: &str,
    uniqueid: &str,
    name: &str,
    server_cert: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + CLIENT_OPEN_WAIT;
    loop {
        match client.add_host(address, uniqueid, name, server_cert).await {
            Ok(()) => return Ok(()),
            Err(MoonlightError::ClientOpen) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(RETRY_EVERY).await;
            }
            Err(err @ MoonlightError::ClientOpen) => return Err(err.to_string()),
            Err(err) => {
                eprintln!("dusk: the client could not record {name} as paired: {err}");
                return Err(SAVE_FAILED.into());
            }
        }
    }
}

/// Wait for a host that recorded this machine while it was streaming to be
/// ready, which is when it reports this machine as paired. No deadline while
/// it is still hosting: that stream is someone's, and may run for hours.
async fn wait_until_paired(
    state: &Arc<AppState>,
    tls: &reqwest::Client,
    host: &str,
    http_port: u16,
    https_port: u16,
) -> Result<(), String> {
    let mut idle_since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(RETRY_EVERY).await;
        // Over TLS with this machine's certificate, `PairStatus` is the
        // answer. A host that does not trust it yet refuses it there, so
        // whether it is busy comes from the plain port.
        if let Ok(info) = serverinfo::query_https(tls, host, https_port).await {
            if info.pairing() == PairingState::Paired {
                return Ok(());
            }
        }
        let hosting = serverinfo::query_http(&state.http, host, http_port)
            .await
            .is_ok_and(|info| matches!(info.activity(), Activity::Hosting { .. }));
        if hosting {
            idle_since = tokio::time::Instant::now();
        } else if idle_since.elapsed() > IDLE_WAIT {
            return Err(NOT_FINISHED.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{CLIENT_CERT, SERVER_CERT};
    use super::*;
    use crate::account::keys::{cert_reply_message, verify, MachineKey};

    const OTHER_CERT: &str = "-----BEGIN CERTIFICATE-----\nOTHER\n-----END CERTIFICATE-----\n";

    fn reply(key: &MachineKey, signed_for: &str, uniqueid: &str) -> CertOutcome {
        CertOutcome::Paired {
            server_cert: SERVER_CERT.into(),
            uniqueid: uniqueid.into(),
            ready: true,
            proof: Proof {
                account: 42,
                machine: key.id.clone(),
                signature: key.sign(&cert_reply_message(signed_for, "HOST-ID", SERVER_CERT)),
            },
        }
    }

    /// Verifies against `key` alone, as the account's machine list would.
    fn by(key: &MachineKey) -> impl FnOnce(Proof, Vec<u8>) -> std::future::Ready<bool> {
        let public = key.public_hex();
        move |proof, message| std::future::ready(verify(&public, &message, &proof.signature))
    }

    #[tokio::test]
    async fn a_reply_signed_for_another_client_is_refused() {
        let key = MachineKey::generate().unwrap();
        let theirs = reply(&key, OTHER_CERT, "HOST-ID");
        assert_eq!(
            accept_reply(CLIENT_CERT, "HOST-ID", theirs, by(&key)).await,
            None
        );

        let ours = reply(&key, CLIENT_CERT, "HOST-ID");
        assert_eq!(
            accept_reply(CLIENT_CERT, "HOST-ID", ours, by(&key)).await,
            Some((SERVER_CERT.to_string(), "HOST-ID".to_string(), true))
        );
    }

    #[tokio::test]
    async fn a_reply_naming_another_host_is_refused() {
        let key = MachineKey::generate().unwrap();
        // Signed correctly, but for a host other than the one asked.
        let elsewhere = reply(&key, CLIENT_CERT, "OTHER-HOST");
        assert_eq!(
            accept_reply(CLIENT_CERT, "HOST-ID", elsewhere, |_, _| async { true }).await,
            None
        );
    }

    #[tokio::test]
    async fn anything_but_a_paired_reply_is_the_pin() {
        for outcome in [
            CertOutcome::NoPeer,
            CertOutcome::Declined("Dusk did not understand that request.".into()),
        ] {
            assert_eq!(
                accept_reply(CLIENT_CERT, "HOST-ID", outcome, |_, _| async { true }).await,
                None
            );
        }
    }
}

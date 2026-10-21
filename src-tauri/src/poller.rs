//! Liveness polling.
//!
//! Every known machine gets a `serverinfo` query on a fixed interval, or
//! immediately when something asks for a refresh. Addresses are tried in
//! order and the first to answer becomes the primary, which is how a machine
//! reachable on both LAN and VPN settles on whichever path is up.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use tauri::AppHandle;

use crate::account::Membership;
use crate::applist;
use crate::model::{DeviceId, PairingState, Reachability};
use crate::moonlight::hosts::{certificate_changed, KnownHost};
use crate::registry::ProbeOutcome;
use crate::serverinfo;
use crate::state::{emit_snapshot, AppState};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long after pairing again with a machine before trying once more.
const RE_PAIR_EVERY: Duration = Duration::from_secs(10 * 60);

pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            poll_once(&app, &state).await;

            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = state.refresh_requested() => {}
            }
        }
    });
}

pub async fn poll_once(app: &AppHandle, state: &Arc<AppState>) {
    let targets = state.registry.read().await.probe_targets();
    if targets.is_empty() {
        emit_snapshot(app, state).await;
        return;
    }

    // Read once for the whole tick rather than per target: it is behind a
    // lock, and every probe in a tick should agree on which identity is in
    // play anyway.
    let tls_client = state.tls().await;

    // All machines are probed concurrently; one unreachable host must not
    // hold up the rest of the grid.
    // Read every tick rather than once at start: pairing here, by PIN or by
    // certificate, is what writes a host's certificate into this list.
    // `None` when there is no client, or its store could not be read.
    let known = state
        .moonlight
        .as_ref()
        .and_then(|client| crate::moonlight::hosts::read(Some(client)));

    let probes = targets.into_iter().map(|(id, addresses, port, tls_port)| {
        let http = state.http.clone();
        let tls = tls_client.clone();
        async move {
            (
                id,
                probe(&http, tls.as_ref(), &addresses, port, tls_port).await,
            )
        }
    });

    let results = join_all(probes).await;

    // What each host presented, under the id its probe settled on.
    let mut presented: HashMap<DeviceId, Vec<u8>> = HashMap::new();
    {
        let mut registry = state.registry.write().await;
        for (id, (outcome, cert)) in results {
            let probed = match &outcome {
                ProbeOutcome::Reached { info, pairing, .. } => pairing.unwrap_or(info.pairing()),
                ProbeOutcome::Unreachable => PairingState::Unknown,
            };
            let id = registry.apply_probe(&id, outcome);
            // Under the same lock as the probe, so no snapshot taken in
            // between can show a pairing this machine cannot use.
            let stored = stored_cert(known.as_deref(), &id);
            if shown_pairing(probed, stored, cert.as_deref()) != probed {
                registry.withhold_pairing(&id);
            }
            if let Some(cert) = cert {
                presented.insert(id, cert);
            }
        }
    }

    ask_who_they_are(state).await;
    hold_the_account_machines(state).await;
    re_pair_changed_hosts(state, known.as_deref(), &presented).await;

    emit_snapshot(app, state).await;
    fetch_app_lists(app, state).await;
}

/// Fetch app lists for newly paired machines.
///
/// Runs after the snapshot rather than before, so liveness is never held up
/// waiting on a list that only matters once someone opens a device.
async fn fetch_app_lists(app: &AppHandle, state: &Arc<AppState>) {
    let Some(tls) = state.tls().await else { return };
    let targets = state.registry.read().await.applist_targets();
    if targets.is_empty() {
        return;
    }

    let fetches = targets.into_iter().map(|(id, address, port)| {
        let tls = tls.clone();
        async move { (id, applist::query(&tls, &address, port).await) }
    });

    let mut changed = false;
    for (id, result) in join_all(fetches).await {
        match result {
            Ok(apps) if !apps.is_empty() => {
                state.registry.write().await.set_apps(&id, apps);
                changed = true;
            }
            // An empty list is legitimate and will simply be retried; an
            // error is worth a line but not worth failing the tick over.
            Ok(_) => {}
            Err(err) => eprintln!("dusk: could not read app list for {id}: {err}"),
        }
    }

    if changed {
        emit_snapshot(app, state).await;
    }
}

/// Keep a card for every machine on the account, found or not.
///
/// Discovery alone makes the grid a list of what is advertising right now,
/// so a machine of yours that is switched off -- or whose hosting was
/// uninstalled -- simply disappears. That reads as Dusk losing the machine
/// rather than as the machine being off, and it is the whole reason the
/// account's list is the better source of truth for "yours".
async fn hold_the_account_machines(state: &Arc<AppState>) {
    let entries = state.account.machines_on_account().await;
    // Nothing known yet is not the same as owning nothing: before the list
    // is read, leaving the grid as discovery found it is the honest answer.
    if entries.is_empty() {
        return;
    }
    state.account.publish_addresses(&state.http).await;
    let own = state.account.machine_id().await;
    state
        .registry
        .write()
        .await
        .sync_account(&entries, own.as_deref());
}

/// Ask every reachable machine which one it is, and mark the ones that are
/// yours.
///
/// This is what lets the grid be "your machines" rather than "whatever is
/// advertising here": the account's machine list is keyed by machine id and
/// carries no addresses, so the id has to come from the machine itself. A
/// name cannot stand in for it — this Mac calls itself `Michaels-Mini` on
/// the account and `Michaels-Mac-mini` over mDNS.
///
/// Only machines that answered a probe are asked, and only those Dusk has
/// not already placed. A machine that never answers is never asked again on
/// every tick for nothing.
async fn ask_who_they_are(state: &Arc<AppState>) {
    let unknown: Vec<(DeviceId, String)> = state
        .registry
        .read()
        .await
        .devices()
        .into_iter()
        // Claimed-by-name cards are still asked: the name is a fallback, and
        // the id is what should replace it the moment it can be had.
        .filter(|d| !d.is_self && d.machine_id.is_none())
        .filter(|d| matches!(d.reachability, Reachability::Online { .. }))
        .filter_map(|d| d.primary_address.clone().map(|a| (d.id, a)))
        .collect();
    if unknown.is_empty() {
        return;
    }

    let asked = join_all(unknown.into_iter().map(|(id, address)| async move {
        let host = crate::peer::host_part(&address).to_string();
        let answer = crate::peer::identify(&host).await;
        (id, answer)
    }))
    .await;

    for (id, answer) in asked {
        let Some((machine, _name)) = answer else {
            continue;
        };
        let mine = match &machine {
            Some(m) => state.account.knows_machine(m).await,
            None => false,
        };
        state
            .registry
            .write()
            .await
            .set_identity(&id, machine, mine);
    }
}

/// What the client holds for a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored<'a> {
    /// No telling: the client's store could not be read or came back
    /// empty, or the device has no id to look it up by yet.
    Unknown,
    /// The client holds no certificate for the host: it lists the host
    /// without one, or lists other hosts and not this one.
    Missing,
    /// The certificate the client trusts the host by, as PEM.
    Cert(&'a str),
}

impl<'a> Stored<'a> {
    fn cert(self) -> Option<&'a str> {
        match self {
            Stored::Cert(pem) => Some(pem),
            _ => None,
        }
    }
}

/// What the client holds for the host behind `id`. `known` is `None` when
/// the client's store could not be read.
///
/// A host the list leaves out counts as missing only when the list has
/// other hosts in it. A PIN pairing always writes the host into this list,
/// so a list that was found and holds hosts, just not this one, is a
/// pairing the client never finished. An empty list may be the wrong file
/// or a read that silently came back with nothing, so it settles nothing.
///
/// Matched without regard to case: the client and the host both write the
/// id upper case, so this agrees with the registry's exact match on every
/// id seen so far, and a host that wrote it otherwise must not lose a PIN
/// pairing over it.
fn stored_cert<'a>(known: Option<&'a [KnownHost]>, id: &str) -> Stored<'a> {
    let (Some(known), Some(uniqueid)) = (known, crate::registry::unique_id_of(id)) else {
        return Stored::Unknown;
    };
    if known.is_empty() {
        return Stored::Unknown;
    }
    let host = known.iter().find(|host| {
        host.uuid
            .as_deref()
            .is_some_and(|uuid| uuid.eq_ignore_ascii_case(uniqueid))
    });
    match host {
        None => Stored::Missing,
        Some(host) => host
            .server_cert
            .as_deref()
            .map_or(Stored::Missing, Stored::Cert),
    }
}

/// What the card says about pairing, given what the probe said.
///
/// A host can trust this machine while the client cannot trust it back:
/// the host half of a pairing by certificate done and the client half not,
/// or a host that has since minted a new certificate. Either way a stream
/// would fail, so the card must not say paired. A PIN pairing always leaves
/// the client holding the host's certificate, so it reads as it always did.
///
/// Only a settled answer demotes: the client holding no certificate for
/// the host, or one that differs. Anything Dusk cannot tell
/// leaves the probe's verdict standing.
fn shown_pairing(
    probed: PairingState,
    stored: Stored<'_>,
    presented: Option<&[u8]>,
) -> PairingState {
    if probed != PairingState::Paired {
        return probed;
    }
    match (stored, presented) {
        (Stored::Missing, _) => PairingState::NotPaired,
        // A stored PEM that does not decode counts as unchanged, on purpose:
        // the conservative answer when there is nothing to compare.
        (Stored::Cert(stored), Some(presented)) if certificate_changed(stored, presented) => {
            PairingState::NotPaired
        }
        _ => PairingState::Paired,
    }
}

/// Whether to pair again with a host: only one of your own, and only when
/// it is presenting a certificate other than the one the client trusts.
fn should_re_pair(on_account: bool, stored: Option<&str>, presented: Option<&[u8]>) -> bool {
    on_account
        && matches!(
            (stored, presented),
            (Some(stored), Some(presented)) if certificate_changed(stored, presented)
        )
}

/// When each machine was last paired again, and which are being now.
#[derive(Default)]
pub struct RePairs(Mutex<RePairLog>);

#[derive(Default)]
struct RePairLog {
    started: HashMap<DeviceId, Instant>,
    running: HashSet<DeviceId>,
}

impl RePairs {
    /// Claim a try at pairing again with `id`. False while one is still
    /// running, which can be a long time, and for ten minutes after the
    /// last one started.
    fn begin(&self, id: &str, now: Instant) -> bool {
        let mut log = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if log.running.contains(id) {
            return false;
        }
        if let Some(last) = log.started.get(id) {
            if now.saturating_duration_since(*last) < RE_PAIR_EVERY {
                return false;
            }
        }
        log.started.insert(id.to_string(), now);
        log.running.insert(id.to_string());
        true
    }

    fn end(&self, id: &str) {
        let mut log = self.0.lock().unwrap_or_else(|e| e.into_inner());
        log.running.remove(id);
    }
}

/// Ends a re-pair when dropped.
struct Running<'a>(&'a RePairs, &'a str);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.end(self.1);
    }
}

/// Pair again, without asking, with any machine of yours that has started
/// presenting a new certificate.
///
/// The host still trusts this machine, so its card would read as paired,
/// but the client pins the certificate it paired with and would refuse every
/// stream. The card reads as unpaired until this succeeds, and a host that
/// declines is simply left that way.
///
/// Spawned, never awaited: a pairing can wait minutes for a stream window
/// here to close, and for as long as the host is streaming to someone.
async fn re_pair_changed_hosts(
    state: &Arc<AppState>,
    known: Option<&[KnownHost]>,
    presented: &HashMap<DeviceId, Vec<u8>>,
) {
    if presented.is_empty() {
        return;
    }
    // The cached list only: background work must never be what puts a
    // keystore prompt on screen.
    let Membership::Machines(account) = state.account.known_membership().await else {
        return;
    };
    let devices = state.registry.read().await.devices();
    for device in devices {
        let Some(cert) = presented.get(&device.id) else {
            continue;
        };
        let on_account = device
            .machine_id
            .as_deref()
            .is_some_and(|machine| account.contains(machine));
        if !should_re_pair(
            on_account,
            stored_cert(known, &device.id).cert(),
            Some(cert),
        ) {
            continue;
        }
        let Some(address) = device.primary_address.clone() else {
            continue;
        };
        if !state.repairs.begin(&device.id, Instant::now()) {
            continue;
        }

        let state = state.clone();
        let id = device.id.clone();
        let name = device.name.clone();
        tauri::async_runtime::spawn(async move {
            // Cleared however this ends, panic included, or the machine
            // could never be paired again without restarting Dusk.
            let _running = Running(&state.repairs, &id);
            eprintln!("dusk: {name} has a new certificate; pairing again");
            let paired = crate::cert_pairing::client::pair(
                &state,
                &address,
                &id,
                || eprintln!("dusk: pairing with {id} again once the stream window here closes"),
                || eprintln!("dusk: pairing with {id} again once it stops streaming"),
            )
            .await;
            match paired {
                Ok(true) => state.request_refresh(),
                Ok(false) => eprintln!("dusk: {id} did not pair again by certificate"),
                Err(err) => eprintln!("dusk: could not pair again with {id}: {err}"),
            }
        });
    }
}

/// Try each address in turn, preferring the authenticated probe.
///
/// Three outcomes matter, not two:
///
/// - TLS answers `200` — the best case. Full detail *and* real pairing state.
/// - TLS answers `401` — the host rejected our certificate. That is a real
///   "not paired", but the body carries no hostname or session state, so the
///   plain probe still runs for detail and the pairing verdict is carried
///   over. A host that answers only this is still online.
/// - TLS fails outright — plain probe alone, pairing unknown.
///
/// Alongside the outcome comes the certificate the host presented over TLS,
/// whatever it then said about us.
async fn probe(
    http: &reqwest::Client,
    tls: Option<&reqwest::Client>,
    addresses: &[String],
    port: u16,
    tls_port: u16,
) -> (ProbeOutcome, Option<Vec<u8>>) {
    let mut rejected: Option<(String, serverinfo::ServerInfo)> = None;

    for address in addresses {
        if let Some(tls) = tls {
            if let Ok(info) = serverinfo::query_https(tls, address, tls_port).await {
                if info.status_code == serverinfo::STATUS_OK {
                    let cert = info.server_cert.clone();
                    let outcome = ProbeOutcome::Reached {
                        address: address.clone(),
                        info,
                        pairing: None,
                    };
                    return (outcome, cert);
                }
                if info.status_code == serverinfo::STATUS_UNAUTHORIZED && rejected.is_none() {
                    rejected = Some((address.clone(), info));
                }
            }
        }

        if let Ok(info) = serverinfo::query_http(http, address, port).await {
            let cert = rejected.as_ref().and_then(|(_, r)| r.server_cert.clone());
            let outcome = ProbeOutcome::Reached {
                address: address.clone(),
                // Carry the authenticated verdict over the plain probe's
                // Unknown, which is strictly less informative.
                pairing: rejected.as_ref().map(|(_, r)| r.pairing()),
                info,
            };
            return (outcome, cert);
        }
    }

    // Reachable over TLS but not over the plain port: still online, and we
    // know exactly where we stand on pairing.
    match rejected {
        Some((address, info)) => {
            let cert = info.server_cert.clone();
            let outcome = ProbeOutcome::Reached {
                address,
                pairing: Some(info.pairing()),
                info,
            };
            (outcome, cert)
        }
        None => (ProbeOutcome::Unreachable, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pem(der: &[u8]) -> String {
        use base64::Engine;
        let body = base64::engine::general_purpose::STANDARD.encode(der);
        format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
    }

    const STORED: &[u8] = b"the certificate paired with";
    const MINTED: &[u8] = b"a certificate minted since";

    #[test]
    fn a_machine_off_the_account_is_never_re_paired() {
        let stored = pem(STORED);
        assert!(!should_re_pair(false, Some(&stored), Some(MINTED)));
        // The same change on one of yours is exactly the case to act on.
        assert!(should_re_pair(true, Some(&stored), Some(MINTED)));
        // Nothing changed, or nothing to compare: nothing to do.
        assert!(!should_re_pair(true, Some(&stored), Some(STORED)));
        assert!(!should_re_pair(true, None, Some(MINTED)));
        assert!(!should_re_pair(true, Some(&stored), None));
    }

    #[test]
    fn a_re_pair_is_not_retried_more_than_once_in_ten_minutes() {
        let repairs = RePairs::default();
        let start = Instant::now();
        assert!(repairs.begin("sunshine:A", start));
        // Not while the first is still going, however long it waits.
        assert!(!repairs.begin("sunshine:A", start + Duration::from_secs(1)));
        repairs.end("sunshine:A");
        assert!(!repairs.begin("sunshine:A", start + Duration::from_secs(9 * 60)));
        // Another machine has its own allowance.
        assert!(repairs.begin("sunshine:B", start + Duration::from_secs(60)));
        assert!(repairs.begin("sunshine:A", start + Duration::from_secs(10 * 60)));
    }

    #[test]
    fn a_host_trusting_us_that_we_cannot_trust_back_is_not_shown_paired() {
        let stored = pem(STORED);
        // The client remembers the host but holds no certificate for it.
        assert_eq!(
            shown_pairing(PairingState::Paired, Stored::Missing, Some(STORED)),
            PairingState::NotPaired
        );
        // The client holds a certificate the host no longer presents.
        assert_eq!(
            shown_pairing(PairingState::Paired, Stored::Cert(&stored), Some(MINTED)),
            PairingState::NotPaired
        );
        // A PIN pairing: both halves agree, and nothing changes.
        assert_eq!(
            shown_pairing(PairingState::Paired, Stored::Cert(&stored), Some(STORED)),
            PairingState::Paired
        );
        // Anything the probe did not call paired is left as it said.
        for probed in [PairingState::NotPaired, PairingState::Unknown] {
            assert_eq!(shown_pairing(probed, Stored::Missing, Some(MINTED)), probed);
        }
    }

    fn host(uuid: &str, server_cert: Option<&str>) -> KnownHost {
        KnownHost {
            name: "venus".into(),
            addresses: vec!["192.168.50.149".into()],
            port: crate::model::DEFAULT_HTTP_PORT,
            uuid: Some(uuid.into()),
            server_cert: server_cert.map(str::to_string),
        }
    }

    #[test]
    fn a_store_that_could_not_be_read_leaves_paired_standing() {
        // A failed read says nothing about the host; demoting on it would
        // unpair every PIN-paired machine on a platform whose read broke.
        let stored = stored_cert(None, "sunshine:ABC");
        assert_eq!(stored, Stored::Unknown);
        assert_eq!(
            shown_pairing(PairingState::Paired, stored, Some(STORED)),
            PairingState::Paired
        );
    }

    #[test]
    fn a_host_missing_from_the_clients_list_is_not_shown_paired() {
        // A PIN pairing always writes the host into the client's list, so a
        // list that has hosts but not this one is the client half missing.
        let known = [host("OTHER", None)];
        let stored = stored_cert(Some(&known), "sunshine:ABC");
        assert_eq!(stored, Stored::Missing);
        assert_eq!(
            shown_pairing(PairingState::Paired, stored, Some(STORED)),
            PairingState::NotPaired
        );
    }

    #[test]
    fn an_empty_list_leaves_paired_standing() {
        // Empty may be the wrong file, or a platform read that came back
        // with nothing; neither says anything about this host.
        let stored = stored_cert(Some(&[]), "sunshine:ABC");
        assert_eq!(stored, Stored::Unknown);
        assert_eq!(
            shown_pairing(PairingState::Paired, stored, Some(STORED)),
            PairingState::Paired
        );
    }

    #[test]
    fn a_device_without_its_id_yet_leaves_paired_standing() {
        let known = [host("OTHER", None)];
        let stored = stored_cert(Some(&known), "addr:192.168.50.149:47989");
        assert_eq!(stored, Stored::Unknown);
        assert_eq!(
            shown_pairing(PairingState::Paired, stored, Some(STORED)),
            PairingState::Paired
        );
    }

    #[test]
    fn the_stored_certificate_is_found_by_uuid_whatever_its_case() {
        let stored = pem(STORED);
        let known = [host("abc-1", Some(&stored)), host("DEF-2", None)];
        assert_eq!(
            stored_cert(Some(&known), "sunshine:ABC-1"),
            Stored::Cert(&stored)
        );
        assert_eq!(stored_cert(Some(&known), "sunshine:DEF-2"), Stored::Missing);
    }
}

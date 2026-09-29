//! Liveness polling.
//!
//! Every known machine gets a `serverinfo` query on a fixed interval, or
//! immediately when something asks for a refresh. Addresses are tried in
//! order and the first to answer becomes the primary, which is how a machine
//! reachable on both LAN and VPN settles on whichever path is up.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use tauri::AppHandle;

use crate::applist;
use crate::registry::ProbeOutcome;
use crate::serverinfo;
use crate::state::{emit_snapshot, AppState};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

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

    {
        let mut registry = state.registry.write().await;
        for (id, outcome) in results {
            registry.apply_probe(&id, outcome);
        }
    }

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
async fn probe(
    http: &reqwest::Client,
    tls: Option<&reqwest::Client>,
    addresses: &[String],
    port: u16,
    tls_port: u16,
) -> ProbeOutcome {
    let mut rejected: Option<(String, serverinfo::ServerInfo)> = None;

    for address in addresses {
        if let Some(tls) = tls {
            if let Ok(info) = serverinfo::query_https(tls, address, tls_port).await {
                if info.status_code == serverinfo::STATUS_OK {
                    return ProbeOutcome::Reached {
                        address: address.clone(),
                        info,
                        pairing: None,
                    };
                }
                if info.status_code == serverinfo::STATUS_UNAUTHORIZED && rejected.is_none() {
                    rejected = Some((address.clone(), info));
                }
            }
        }

        if let Ok(info) = serverinfo::query_http(http, address, port).await {
            return ProbeOutcome::Reached {
                address: address.clone(),
                // Carry the authenticated verdict over the plain probe's
                // Unknown, which is strictly less informative.
                pairing: rejected.as_ref().map(|(_, r)| r.pairing()),
                info,
            };
        }
    }

    // Reachable over TLS but not over the plain port: still online, and we
    // know exactly where we stand on pairing.
    match rejected {
        Some((address, info)) => ProbeOutcome::Reached {
            address,
            pairing: Some(info.pairing()),
            info,
        },
        None => ProbeOutcome::Unreachable,
    }
}

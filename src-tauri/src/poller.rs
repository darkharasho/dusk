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

    // All machines are probed concurrently; one unreachable host must not
    // hold up the rest of the grid.
    let probes = targets.into_iter().map(|(id, addresses, port)| {
        let http = state.http.clone();
        async move { (id, probe(&http, &addresses, port).await) }
    });

    let results = join_all(probes).await;

    {
        let mut registry = state.registry.write().await;
        for (id, outcome) in results {
            registry.apply_probe(&id, outcome);
        }
    }

    emit_snapshot(app, state).await;
}

async fn probe(client: &reqwest::Client, addresses: &[String], port: u16) -> ProbeOutcome {
    for address in addresses {
        if let Ok(info) = serverinfo::query_http(client, address, port).await {
            return ProbeOutcome::Reached {
                address: address.clone(),
                info,
            };
        }
    }
    ProbeOutcome::Unreachable
}

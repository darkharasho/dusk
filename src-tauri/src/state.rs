//! Shared application state and the one way the frontend learns about it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::host::HostBackend;
use crate::model::Snapshot;
use crate::moonlight::{ClientIdentity, Moonlight};
use crate::registry::Registry;
use crate::store::Store;
use crate::sunshine::CredentialStore;

pub const SNAPSHOT_EVENT: &str = "dusk://snapshot";

pub struct AppState {
    pub registry: RwLock<Registry>,
    pub host: Box<dyn HostBackend>,
    pub store: Mutex<Store>,
    pub store_path: PathBuf,
    pub http: reqwest::Client,
    /// Presents our client certificate, so `PairStatus` is meaningful.
    ///
    /// `None` until moonlight-qt has paired with something at least once,
    /// and swappable because the first successful pair is what creates the
    /// identity — see [`AppState::reload_identity`].
    tls: RwLock<Option<reqwest::Client>>,
    /// `None` when moonlight-qt is not installed.
    pub moonlight: Option<Moonlight>,
    /// This machine's Sunshine web-UI sign-in, in the OS keystore.
    pub credentials: CredentialStore,
    discovering: AtomicBool,
    refresh: Notify,
}

/// Build a client that presents the Moonlight identity.
///
/// `danger_accept_invalid_certs` is load-bearing and not laziness: Sunshine
/// serves a self-signed certificate, so ordinary verification can only fail.
/// Moonlight's own answer is to pin the certificate it was handed at pairing,
/// and Dusk should do the same once it drives pairing — until then this
/// endpoint is read-only and carries nothing secret, but it is a real gap and
/// it closes with the pairing work.
pub fn tls_client(identity: &ClientIdentity) -> Option<reqwest::Client> {
    let pem = identity.to_combined_pem();
    let id = match reqwest::Identity::from_pem(&pem) {
        Ok(id) => id,
        Err(err) => {
            eprintln!("dusk: moonlight identity unusable, falling back to plain probes: {err}");
            return None;
        }
    };

    reqwest::Client::builder()
        .identity(id)
        .danger_accept_invalid_certs(true)
        .connect_timeout(Duration::from_millis(1500))
        .build()
        .map_err(|err| eprintln!("dusk: could not build TLS client: {err}"))
        .ok()
}

impl AppState {
    pub fn new(
        registry: Registry,
        host: Box<dyn HostBackend>,
        store: Store,
        store_path: PathBuf,
        http: reqwest::Client,
        tls: Option<reqwest::Client>,
        moonlight: Option<Moonlight>,
    ) -> Self {
        Self {
            registry: RwLock::new(registry),
            host,
            store: Mutex::new(store),
            store_path,
            http,
            tls: RwLock::new(tls),
            moonlight,
            credentials: CredentialStore::default(),
            discovering: AtomicBool::new(false),
            refresh: Notify::new(),
        }
    }

    /// The authenticated client, if we have an identity.
    pub async fn tls(&self) -> Option<reqwest::Client> {
        self.tls.read().await.clone()
    }

    /// Re-read moonlight-qt's identity and rebuild the TLS client.
    ///
    /// Called after pairing: a first-ever pair mints the identity, so until
    /// this runs Dusk would keep probing unauthenticated and reporting the
    /// machine it just paired with as unknown.
    pub fn reload_identity(self: &Arc<Self>) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            let rebuilt = crate::moonlight::identity::load().and_then(|id| tls_client(&id));
            if rebuilt.is_some() {
                *state.tls.write().await = rebuilt;
            }
        });
    }

    pub fn set_discovering(&self, value: bool) {
        self.discovering.store(value, Ordering::Relaxed);
    }

    pub fn is_discovering(&self) -> bool {
        self.discovering.load(Ordering::Relaxed)
    }

    /// Ask the poller to run now instead of waiting for the next tick.
    pub fn request_refresh(&self) {
        self.refresh.notify_one();
    }

    pub async fn refresh_requested(&self) {
        self.refresh.notified().await;
    }

    pub async fn snapshot(&self) -> Snapshot {
        Snapshot {
            devices: self.registry.read().await.devices(),
            host: self.host.state().await,
            discovering: self.is_discovering(),
            moonlight_available: self.moonlight.is_some(),
            host_signed_in: self.credentials.load().await.is_some(),
            host_credentials_persistent: self.credentials.is_persistent().await,
        }
    }
}

/// Push the full state to the frontend. Cheap enough at this scale that
/// diffing would be premature, and it keeps the UI a pure function of one
/// payload.
pub async fn emit_snapshot(app: &AppHandle, state: &Arc<AppState>) {
    let snapshot = state.snapshot().await;
    if let Err(err) = app.emit(SNAPSHOT_EVENT, snapshot) {
        eprintln!("dusk: could not emit snapshot: {err}");
    }
}

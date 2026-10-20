//! Shared application state and the one way the frontend learns about it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::host::HostBackend;
use crate::model::{DeviceId, Snapshot};
use crate::moonlight::{ClientIdentity, Moonlight};
use crate::registry::Registry;
use crate::store::Store;
use crate::sunshine::{CredentialStore, Known};

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
    /// A request from another Dusk to pair with this machine, waiting on
    /// someone here to allow it.
    pub peer: crate::peer::Requests,
    /// The GitHub sign-in that lets this machine's own other machines pair
    /// without being asked.
    pub account: crate::account::AccountState,
    /// Devices this Dusk currently has a Moonlight session open with.
    ///
    /// The host cannot tell us this: Sunshine reports that *a* session
    /// exists, not whose, and it keeps one open after its client goes
    /// away. Only the side that launched the client knows.
    ///
    /// Each carries the way to talk to that stream's client, for the
    /// controls in Dusk's own window.
    sessions: RwLock<HashMap<DeviceId, crate::session::Control>>,
    /// mDNS is being browsed. It always is once started, so this alone is
    /// not something to show.
    browsing: AtomicBool,
    /// A sweep is under way: the first seconds of browsing, or the seconds
    /// after a Refresh. This is what the window shows as looking.
    discovering: AtomicBool,
    /// Bumped by each sweep, so an earlier sweep ending does not cut short
    /// one started after it.
    sweeps: AtomicU64,
    refresh: Notify,
}

/// The settings both HTTP clients need, whatever they talk to Sunshine over.
///
/// `pool_max_idle_per_host(0)` is the load-bearing one, and it was measured
/// rather than guessed. Sunshine answers without `Connection: close` and then
/// closes the socket server-side anyway, on the plain port and the TLS port
/// alike:
///
/// ```text
/// * Connection #0 to host 192.168.50.146 left intact
/// * Connection 0 seems to be dead
/// ```
///
/// curl notices and silently reconnects. hyper's pool keeps the dead socket
/// and whether the next request notices in time is a race, so a poll every
/// five seconds fails intermittently with nothing but "error sending
/// request" — which reads like an unreachable host and is not. Pooling buys
/// nothing against a server that closes every connection, so it is off.
fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(1500))
        .pool_max_idle_per_host(0)
}

/// An unauthenticated client for the plain GameStream port.
pub fn plain_client() -> reqwest::Result<reqwest::Client> {
    client_builder().build()
}

/// Accept whatever certificate Sunshine presents.
///
/// Not laziness: Sunshine serves a self-signed certificate, so ordinary
/// verification can only ever fail. Moonlight's own answer is to pin the
/// certificate it was handed at pairing, and Dusk should do the same once it
/// drives pairing — until then this endpoint is read-only and carries
/// nothing secret, but it is a real gap and it closes with the pairing work.
#[derive(Debug)]
struct AcceptAnyServerCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    // The signature checks stay real. Only the certificate's provenance is
    // waived; a handshake that does not actually hold the key still fails.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Build a client that presents the Moonlight identity.
///
/// # Why the TLS config is built by hand
///
/// Sunshine aborts a *resumed* handshake. Measured against a real host, one
/// reqwest client reusing its rustls session cache goes: first request fine,
/// the next two killed with `received fatal alert: InternalError`, then one
/// fine again as rustls gives up on the ticket and does a full handshake.
/// Eight requests through eight fresh clients succeed eight times. So the
/// server is healthy and the session cache is the whole problem.
///
/// Capping to TLS 1.2 makes it worse rather than better, so this is not a
/// 1.3-ticket quirk. reqwest exposes no resumption knob, which is why the
/// `rustls::ClientConfig` is assembled here instead of through the builder.
pub fn tls_client(identity: &ClientIdentity) -> Option<reqwest::Client> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let certs = rustls_pemfile::certs(&mut identity.cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let key = rustls_pemfile::private_key(&mut identity.key_pem.as_slice())
        .ok()
        .flatten()?;

    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
        .with_client_auth_cert(certs, key)
        .map_err(|err| {
            eprintln!("dusk: moonlight identity unusable, falling back to plain probes: {err}")
        })
        .ok()?;

    config.resumption = rustls::client::Resumption::disabled();

    client_builder()
        .use_preconfigured_tls(config)
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
            peer: Default::default(),
            account: Default::default(),
            sessions: RwLock::new(HashMap::new()),
            browsing: AtomicBool::new(false),
            discovering: AtomicBool::new(false),
            sweeps: AtomicU64::new(0),
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
            let rebuilt = crate::moonlight::identity::load(state.moonlight.as_ref())
                .and_then(|id| tls_client(&id));
            if rebuilt.is_some() {
                *state.tls.write().await = rebuilt;
            }
        });
    }

    pub fn set_browsing(&self, value: bool) {
        self.browsing.store(value, Ordering::Relaxed);
        if !value {
            self.discovering.store(false, Ordering::Relaxed);
        }
    }

    pub fn is_browsing(&self) -> bool {
        self.browsing.load(Ordering::Relaxed)
    }

    /// Start a sweep, returning the ticket that ends it.
    pub fn begin_sweep(&self) -> u64 {
        self.discovering.store(true, Ordering::Relaxed);
        self.sweeps.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// End a sweep if no later one has started. Returns whether it ended.
    pub fn end_sweep(&self, ticket: u64) -> bool {
        if self.sweeps.load(Ordering::Relaxed) != ticket {
            return false;
        }
        self.discovering.store(false, Ordering::Relaxed);
        true
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

    /// Note that a stream to this device has started or ended here.
    pub async fn session_started(&self, id: &str, control: crate::session::Control) {
        self.sessions.write().await.insert(id.to_string(), control);
    }

    pub async fn session_ended(&self, id: &str) {
        self.sessions.write().await.remove(id);
    }

    /// The way to talk to the stream running from this device, if there is
    /// one. Whether its client is listening is another matter.
    pub async fn session_control(&self, id: &str) -> Option<crate::session::Control> {
        self.sessions.read().await.get(id).cloned()
    }

    pub async fn snapshot(&self) -> Snapshot {
        let sessions: std::collections::HashSet<DeviceId> =
            self.sessions.read().await.keys().cloned().collect();
        let mut devices = self.registry.read().await.devices();
        for device in &mut devices {
            // Or-ed rather than assigned: mock fixtures set this directly
            // to stage a mid-session card, and there is no real client to
            // put them in the live set.
            device.streaming_here |= sessions.contains(&device.id);
        }

        Snapshot {
            devices,
            host: self.host.state().await,
            discovering: self.is_discovering(),
            moonlight_available: self.moonlight.is_some(),
            overlay_available: self.moonlight.as_ref().and_then(Moonlight::overlay),
            // `peek`, never `load`: a snapshot is on the path to drawing the
            // window, and a keystore read can block behind a password prompt.
            host_signed_in: match self.credentials.peek().await {
                Known::Unknown => None,
                Known::SignedIn => Some(true),
                Known::SignedOut => Some(false),
            },
            host_credentials_persistent: self.credentials.is_persistent().await,
            allow_other_devices: self.store.lock().await.allow_other_devices,
            resolution: self.store.lock().await.resolution,
        }
    }
}

/// Push the full state to the frontend. Cheap enough at this scale that
/// diffing would be premature, and it keeps the UI a pure function of one
/// payload.
pub async fn emit_snapshot(app: &AppHandle, state: &Arc<AppState>) {
    let snapshot = state.snapshot().await;

    // The tray menu is another view of the same payload, and updating it here
    // is what keeps it from drifting out of step with the window.
    crate::tray::reflect(app, &snapshot);

    if let Err(err) = app.emit(SNAPSHOT_EVENT, snapshot) {
        eprintln!("dusk: could not emit snapshot: {err}");
    }
}

//! Shared application state and the one way the frontend learns about it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::host::HostBackend;
use crate::model::{DeviceId, Snapshot};
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
    /// Devices this Dusk currently has a Moonlight session open with.
    ///
    /// The host cannot tell us this: Sunshine reports that *a* session
    /// exists, not whose, and it keeps one open after its client goes
    /// away. Only the side that launched the client knows.
    sessions: RwLock<HashSet<DeviceId>>,
    discovering: AtomicBool,
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
            sessions: RwLock::new(HashSet::new()),
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

    /// Note that a stream to this device has started or ended here.
    pub async fn set_session_active(&self, id: &str, active: bool) {
        let mut sessions = self.sessions.write().await;
        if active {
            sessions.insert(id.to_string());
        } else {
            sessions.remove(id);
        }
    }

    pub async fn snapshot(&self) -> Snapshot {
        let sessions = self.sessions.read().await.clone();
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

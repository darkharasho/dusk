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

/// A stream running from this machine: how to talk to its client, and how
/// to end it when talking does not.
#[derive(Clone)]
pub struct Running {
    pub control: crate::session::Control,
    /// The client's process, for a quit that it does not answer.
    pub pid: Option<u32>,
}

/// How long a stream is given to end on its own when Dusk quits, before
/// it is made to; and then again before Dusk stops waiting.
pub const QUIT_GRACE: Duration = Duration::from_secs(3);

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
    /// Keeps the host's list of paired machines in line with the account.
    pub reconciler: crate::cert_pairing::reconcile::Reconciler,
    /// Pairing again with machines of yours whose certificate changed.
    pub repairs: crate::poller::RePairs,
    /// Devices this Dusk currently has a Moonlight session open with.
    ///
    /// The host cannot tell us this: Sunshine reports that *a* session
    /// exists, not whose, and it keeps one open after its client goes
    /// away. Only the side that launched the client knows.
    ///
    /// Each carries the way to talk to that stream's client, for the
    /// controls in Dusk's own window.
    sessions: RwLock<HashMap<DeviceId, Running>>,
    /// How many times clipboard sharing has been turned off here, bumped
    /// under the store lock. A copy held for the next clipboard connection,
    /// or a connection started earlier, belongs to the count it began at.
    pub clipboard_turned_off: AtomicU64,
    /// Set once quitting has begun, so the exit the clean-up itself asks
    /// for is not taken as another request to clean up.
    pub quitting: AtomicBool,
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

    // `tls_info` keeps the certificate the host presented on each response,
    // which is how the poller notices a host that minted a new one. reqwest
    // honours it for a preconfigured rustls config as for its own.
    client_builder()
        .use_preconfigured_tls(config)
        .tls_info(true)
        .build()
        .map_err(|err| eprintln!("dusk: could not build TLS client: {err}"))
        .ok()
}

/// A client for the pairing port with an identity made up on the spot.
///
/// What the host's watchdog asks its own pairing port with. Only whether
/// the port answers matters, but Sunshine requires *a* client certificate
/// to complete the handshake at all: offered none, a healthy host fails the
/// handshake, which from outside looks the same as a dead one. Offered one
/// it has never seen, it answers "not authorized" over HTTP, which is an
/// answer. So the watchdog mints one. Nothing is ever paired with it, and
/// it goes when Dusk does.
pub fn loopback_tls_client() -> Option<reqwest::Client> {
    let minted = rcgen::generate_simple_self_signed(vec!["dusk-watchdog".to_string()])
        .map_err(|err| eprintln!("dusk: could not mint the watchdog's certificate: {err}"))
        .ok()?;
    let identity = ClientIdentity {
        cert_pem: minted.cert.pem().into_bytes(),
        key_pem: minted.key_pair.serialize_pem().into_bytes(),
    };
    tls_client(&identity)
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
        // Shared, so a change to the account wakes the reconciler without
        // the account knowing anything about it.
        let account_changed = Arc::new(Notify::new());
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
            account: crate::account::AccountState::nudging(account_changed.clone()),
            reconciler: crate::cert_pairing::reconcile::Reconciler::new(account_changed),
            repairs: Default::default(),
            sessions: RwLock::new(HashMap::new()),
            clipboard_turned_off: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
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
    pub async fn session_started(
        &self,
        id: &str,
        control: crate::session::Control,
        pid: Option<u32>,
    ) {
        self.sessions
            .write()
            .await
            .insert(id.to_string(), Running { control, pid });
    }

    /// End every stream running from here, because Dusk is quitting.
    ///
    /// A stream left running would outlive the one thing that can end it:
    /// the overlay's Disconnect goes through Dusk, and with Dusk gone it
    /// does nothing, leaving a full-screen stream with no way out but the
    /// task manager. So quitting ends streams, the way closing a browser
    /// ends its tabs. Each client is asked first, so it can take its stream
    /// down cleanly, and given `grace` to do so before it is stopped.
    pub async fn end_streams(&self, grace: Duration) {
        let running: Vec<(DeviceId, Running)> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        if running.is_empty() {
            return;
        }
        eprintln!("dusk: ending {} stream(s) to quit", running.len());
        for (_, r) in &running {
            r.control.send(crate::session::ToClient::Disconnect);
        }
        if self.streams_ended_within(grace).await {
            return;
        }
        for (id, r) in &running {
            if !self.sessions.read().await.contains_key(id) {
                continue;
            }
            match r.pid {
                Some(pid) => {
                    eprintln!("dusk: the stream from {id} did not end when asked; stopping it");
                    terminate(pid);
                }
                None => eprintln!("dusk: the stream from {id} did not end when asked"),
            }
        }
        self.streams_ended_within(grace).await;
    }

    /// Whether this machine is streaming from the device `id` right now.
    pub async fn is_streaming_from(&self, id: &str) -> bool {
        self.sessions.read().await.contains_key(id)
    }

    /// Whether this machine is streaming from another one right now.
    pub async fn has_streams(&self) -> bool {
        !self.sessions.read().await.is_empty()
    }

    /// Whether every stream's watcher has reported it ended, within `within`.
    async fn streams_ended_within(&self, within: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if self.sessions.read().await.is_empty() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn session_ended(&self, id: &str) {
        self.sessions.write().await.remove(id);
    }

    /// The way to talk to the stream running from this device, if there is
    /// one. Whether its client is listening is another matter.
    pub async fn session_control(&self, id: &str) -> Option<crate::session::Control> {
        self.sessions
            .read()
            .await
            .get(id)
            .map(|r| r.control.clone())
    }

    pub async fn snapshot(&self) -> Snapshot {
        let sessions: std::collections::HashSet<DeviceId> =
            self.sessions.read().await.keys().cloned().collect();
        let mut devices = self.registry.read().await.devices();
        // Copied out under one short lock. Two `lock().await`s in the
        // literal below would deadlock: the first guard lives until the
        // whole expression ends, so the second waits on it forever.
        let (
            allow_other_devices,
            resolution,
            bitrate_kbps,
            fps,
            codec,
            virtual_screen,
            clipboard_sharing,
            auto_update,
            restart_owed,
        ) = {
            let store = self.store.lock().await;
            (
                store.allow_other_devices,
                store.resolution,
                store.bitrate_kbps,
                store.fps,
                store.codec,
                store.virtual_screen,
                store.clipboard_sharing.is_on(),
                store.auto_update.is_on(),
                store.restart_owed,
            )
        };
        let host_restart = restart_owed.then(|| match self.reconciler.trouble() {
            Some(reason) => format!(
                "Hosting needs to restart to load a change to its paired machines, but \
                 {reason}. Turn hosting off and on again in Settings › Hosting."
            ),
            None => "Hosting will restart to load a change to its paired machines once \
                     nothing is streaming from here."
                .to_string(),
        });
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
            allow_other_devices,
            resolution,
            bitrate_kbps,
            fps,
            codec,
            virtual_screen,
            virtual_screen_mechanism: crate::host::virtual_screen::mechanism(),
            clipboard_sharing,
            auto_update,
            auto_update_possible: crate::updates::unattended::unattended()
                == crate::updates::unattended::Unattended::Possible,
            host_restart,
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

/// Stop a client that did not end its stream when asked.
///
/// A plain termination rather than a kill: the client gets to run its exit
/// handlers, which is what puts a resized screen back and tells the host the
/// session is over.
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: a plain signal to a pid Dusk spawned itself; it cannot
        // touch memory on this side.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watchdog_can_mint_an_identity_the_tls_client_accepts() {
        assert!(loopback_tls_client().is_some());
    }
    use std::collections::HashSet;
    use std::time::Duration;

    fn state() -> AppState {
        AppState::new(
            Registry::new("here".into(), HashSet::new()),
            Box::new(crate::host::mock::MockHost::default()),
            Store::default(),
            PathBuf::from("unused.json"),
            plain_client().unwrap(),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn quitting_with_nothing_streaming_waits_for_nothing() {
        let state = state();
        tokio::time::timeout(Duration::from_millis(100), state.end_streams(QUIT_GRACE))
            .await
            .expect("returned at once");
    }

    /// A client that never hears the ask, as a stock client does not, is
    /// stopped; and Dusk stops waiting once its watcher has let go.
    /// A stand-in for a client that ignores the disconnect it is sent: a
    /// process that sits for half a minute unless something stops it. Picked
    /// per platform so the test reaches `terminate`'s SIGTERM on Unix and its
    /// `taskkill` on Windows, which CI's Windows runner is the only thing
    /// that exercises.
    fn a_stream_that_ignores_disconnect() -> tokio::process::Command {
        #[cfg(unix)]
        {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", "sleep 30"]);
            command
        }
        #[cfg(windows)]
        {
            // Under cmd, so that `/T` has a child to take with it: Dusk's
            // client spawns helpers of its own, and a kill that left them
            // behind would pass a test that only watched the parent.
            let mut command = tokio::process::Command::new("cmd");
            command.args(["/C", "ping -n 31 127.0.0.1 >nul"]);
            command
        }
    }

    #[tokio::test]
    async fn quitting_stops_a_stream_that_does_not_end_when_asked() {
        let state = Arc::new(state());
        let mut child = a_stream_that_ignores_disconnect()
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id();
        state
            .session_started("dev", crate::session::Control::detached(), pid)
            .await;
        let watched = state.clone();
        tokio::spawn(async move {
            let _ = child.wait().await;
            watched.session_ended("dev").await;
        });
        let started = std::time::Instant::now();
        state.end_streams(Duration::from_millis(300)).await;
        assert!(
            state.session_control("dev").await.is_none(),
            "watcher let go"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not wait out the sleep"
        );
    }

    #[tokio::test]
    async fn snapshot_does_not_hang() {
        let state = AppState::new(
            Registry::new("here".into(), HashSet::new()),
            Box::new(crate::host::mock::MockHost::default()),
            Store::default(),
            PathBuf::from("unused.json"),
            plain_client().unwrap(),
            None,
            None,
        );

        // A snapshot that never returns leaves the window on its masthead.
        tokio::time::timeout(Duration::from_secs(5), state.snapshot())
            .await
            .expect("snapshot hung");
    }
}

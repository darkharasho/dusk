//! The host's side of pairing by certificate.
//!
//! Another of your machines sends its certificate, signed for this host.
//! Dusk writes it into the host's list of paired machines and replies with
//! this host's own certificate, so the two can stream with no PIN.
//!
//! The host only reads that list when it starts, and rewrites it from memory
//! whenever it pairs anything itself — dropping entries it never loaded. So
//! the list is kept right by a reconciler rather than a one-off write: it
//! re-merges what Dusk owes the file on a timer and on every change to the
//! account, and restarts the host to load it, but only while nothing is
//! being streamed. Every write is a fresh read and merge, never bytes
//! worked out earlier, because the file may have been rewritten since.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, Notify};

use crate::account::keys::{cert_reply_message, cert_request_message};
use crate::account::{Membership, Proof};
use crate::model::{Activity, DEFAULT_HTTP_PORT};
use crate::peer::Message;
use crate::state::AppState;
use crate::store::AccountPairing;
use crate::sunshine::state_file::{self, normalise_pem, NamedDevice, StateFileError};
use crate::sunshine::{api, Credentials, Known, SunshineApi};

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

/// The parts of the running host a reconcile pass needs, so the waiting can
/// be tested without one.
#[async_trait]
pub trait Host: Send + Sync {
    /// `Some(true)` while streaming, `Some(false)` when idle, `None` when it
    /// does not answer — which is never taken as idle.
    async fn hosting(&self) -> Option<bool>;
    async fn restart(&self) -> Result<(), String>;
}

/// The host on this machine.
struct Local<'a> {
    http: &'a reqwest::Client,
    credentials: &'a Credentials,
}

#[async_trait]
impl Host for Local<'_> {
    async fn hosting(&self) -> Option<bool> {
        let info = crate::serverinfo::query_http(self.http, "127.0.0.1", DEFAULT_HTTP_PORT)
            .await
            .ok()?;
        Some(matches!(info.activity(), Activity::Hosting { .. }))
    }

    async fn restart(&self) -> Result<(), String> {
        SunshineApi::new(api::DEFAULT_PORT)
            .map_err(|e| e.to_string())?
            .restart(self.credentials)
            .await
            .map_err(|e| e.to_string())
    }
}

/// How one pass went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// The file holds something the running host has not loaded yet.
    pub pending: bool,
    /// This pass restarted the host.
    pub restarted: bool,
}

/// Bring the file in line with `add` and `remove`, and restart the host if
/// it is owed one and nothing is streaming.
async fn pass(
    pending: &mut bool,
    path: &Path,
    add: &[NamedDevice],
    remove: &[String],
    host: &dyn Host,
) -> Result<Pass, StateFileError> {
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        state_file::write_atomic(path, &merged)?;
        *pending = true;
    }
    let waiting = Pass {
        pending: *pending,
        restarted: false,
    };
    if !*pending || host.hosting().await != Some(false) {
        return Ok(waiting);
    }
    // Once more, from disk: the host may have rewritten the file while it
    // was being asked, and a restart loads whatever is there now.
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        state_file::write_atomic(path, &merged)?;
    }
    match host.restart().await {
        Ok(()) => {
            *pending = false;
            Ok(Pass {
                pending: false,
                restarted: true,
            })
        }
        Err(err) => {
            eprintln!("dusk: could not restart hosting to load paired machines: {err}");
            Ok(waiting)
        }
    }
}

/// How long the reconciler waits for things.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// Between passes when nothing is owed.
    tick: Duration,
    /// Between passes while a restart is owed.
    poll: Duration,
    /// After a restart, before asking whether the host is back, so the
    /// process on its way out is not what answers.
    settle: Duration,
    /// Between asks while waiting for it to come back.
    retry: Duration,
    /// How long a pairing waits for the host to come back.
    back_within: Duration,
}

impl Timing {
    const REAL: Timing = Timing {
        tick: Duration::from_secs(60),
        poll: Duration::from_secs(15),
        settle: Duration::from_secs(1),
        retry: Duration::from_millis(500),
        back_within: Duration::from_secs(20),
    };
}

/// Run passes forever: on a nudge, on the tick, and on the shorter poll
/// while a restart is owed. `pass` answers whether one still is.
async fn drive<F>(nudged: &Notify, timing: &Timing, mut pass: F)
where
    F: FnMut() -> BoxFuture<'static, bool>,
{
    let mut pending = false;
    loop {
        let wait = if pending { timing.poll } else { timing.tick };
        tokio::select! {
            _ = nudged.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
        pending = pass().await;
    }
}

/// Whether the host answers again within the time a pairing waits.
async fn back_within(host: &dyn Host, timing: &Timing) -> bool {
    tokio::time::timeout(timing.back_within, async {
        tokio::time::sleep(timing.settle).await;
        while host.hosting().await.is_none() {
            tokio::time::sleep(timing.retry).await;
        }
    })
    .await
    .is_ok()
}

/// Where the host keeps the files this needs.
struct Files {
    state: PathBuf,
    server_cert: PathBuf,
}

/// Paths as the host is configured, or its defaults when the config cannot
/// be read: an unset key is simply absent from it.
async fn files(credentials: &Credentials) -> Option<Files> {
    let config = match SunshineApi::new(api::DEFAULT_PORT) {
        Ok(api) => api.get_config(credentials).await.ok(),
        Err(_) => None,
    };
    Some(Files {
        state: state_file::state_path(config.as_ref())?,
        server_cert: state_file::server_cert_path(config.as_ref())?,
    })
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

/// Keeps the host's list of paired machines in line with your account.
pub struct Reconciler {
    nudged: Arc<Notify>,
    /// One pass at a time, and whether a restart is owed. Two read, merge,
    /// write cycles interleaving could each write a file missing the
    /// other's entry.
    pending: Mutex<bool>,
}

impl Reconciler {
    /// `nudged` is shared with whatever should be able to wake it.
    pub fn new(nudged: Arc<Notify>) -> Self {
        Self {
            nudged,
            pending: Mutex::new(false),
        }
    }

    /// Run a pass now rather than at the next tick.
    pub fn nudge(&self) {
        self.nudged.notify_one();
    }

    /// Run passes in the background for as long as Dusk runs.
    ///
    /// The first pass waits for the nudge sent once the keystore has been
    /// read at startup: before then nothing is known, and asking would put
    /// a second keystore prompt beside the first. That pass is what puts
    /// the list right after the host was reinstalled or rewrote its file
    /// while Dusk was not running.
    pub fn spawn(state: Arc<AppState>) {
        tauri::async_runtime::spawn(async move {
            let looping = state.clone();
            drive(&state.reconciler.nudged, &Timing::REAL, move || {
                let state = looping.clone();
                Box::pin(async move { state.reconciler.background(&state).await })
            })
            .await;
        });
    }

    /// One pass from the loop. Answers whether a restart is still owed.
    async fn background(&self, state: &AppState) -> bool {
        // `peek` first: a background pass must never be what puts a keystore
        // prompt on screen.
        let credentials = match state.credentials.peek().await {
            Known::SignedIn => state.credentials.load().await,
            _ => None,
        };
        let Some(credentials) = credentials else {
            return *self.pending.lock().await;
        };
        match self.reconcile(state, &credentials).await {
            Ok(pass) => pass.pending,
            Err(err) => {
                eprintln!("dusk: could not update this machine's paired machines: {err}");
                *self.pending.lock().await
            }
        }
    }

    /// Plan from what is stored now, apply it, and forget pairings the file
    /// no longer has.
    async fn reconcile(
        &self,
        state: &AppState,
        credentials: &Credentials,
    ) -> Result<Pass, StateFileError> {
        let mut pending = self.pending.lock().await;
        let (pairings, retired) = {
            let store = state.store.lock().await;
            (
                store.account_pairings.clone(),
                store.retired_pairings.clone(),
            )
        };
        if pairings.is_empty() && retired.is_empty() && !*pending {
            return Ok(Pass {
                pending: false,
                restarted: false,
            });
        }
        let membership = state.account.membership().await;
        let (add, mut remove) = plan(&pairings, &membership);
        remove.extend(retired);
        let files = files(credentials).await.ok_or(StateFileError::Missing)?;
        let host = Local {
            http: &state.http,
            credentials,
        };
        let outcome = pass(&mut pending, &files.state, &add, &remove, &host).await?;
        if !remove.is_empty() {
            // Whether this pass took them out or they were already gone, the
            // file no longer has them, so neither need Dusk.
            let mut store = state.store.lock().await;
            store.account_pairings.retain(|p| !remove.contains(&p.uuid));
            store.retired_pairings.retain(|u| !remove.contains(u));
            if let Err(err) = store.save(&state.store_path) {
                eprintln!("dusk: could not save paired machines: {err}");
            }
        }
        Ok(outcome)
    }
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

    let outcome = state
        .reconciler
        .reconcile(state, &credentials)
        .await
        .map_err(|err| {
            eprintln!("dusk: could not pair {name} by certificate: {err}");
            "This machine couldn't add yours to its list of paired machines.".to_string()
        })?;
    let ready = if outcome.restarted {
        let host = Local {
            http: &state.http,
            credentials: &credentials,
        };
        back_within(&host, &Timing::REAL).await
    } else {
        if outcome.pending {
            // So the loop moves to its shorter poll and restarts the host as
            // soon as the stream ends.
            state.reconciler.nudge();
        }
        !outcome.pending
    };

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
    use super::*;
    use crate::account::keys::{cert_request_message, MachineKey};
    use crate::account::Membership;
    use crate::store::AccountPairing;
    use crate::sunshine::state_file;
    use std::collections::{HashSet, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    const STATE: &str = r#"{
  "username": "u",
  "root": {
    "uniqueid": "HOST-ID",
    "named_devices": [
      {"name": "Phone", "cert": "PHONE", "uuid": "U1", "enabled": "true"}
    ]
  }
}"#;
    const SERVER_CERT: &str = "-----BEGIN CERTIFICATE-----\nSERVER\n-----END CERTIFICATE-----\n";
    const CLIENT_CERT: &str = "-----BEGIN CERTIFICATE-----\nCLIENT\n-----END CERTIFICATE-----\n";

    /// A Sunshine config directory of its own, removed when the test ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("dusk-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("sunshine_state.json"), STATE).unwrap();
            std::fs::write(dir.join("cacert.pem"), SERVER_CERT).unwrap();
            Self(dir)
        }
        fn files(&self) -> Files {
            Files {
                state: self.0.join("sunshine_state.json"),
                server_cert: self.0.join("cacert.pem"),
            }
        }
        fn state(&self) -> PathBuf {
            self.0.join("sunshine_state.json")
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn uuids(path: &Path) -> Vec<String> {
        state_file::read(path).unwrap()["root"]["named_devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["uuid"].as_str().unwrap().to_string())
            .collect()
    }

    fn pairing(machine: &str, uuid: &str) -> AccountPairing {
        AccountPairing {
            machine: machine.into(),
            name: format!("{machine}'s desk"),
            cert: format!("-----BEGIN CERTIFICATE-----\n{machine}\n-----END CERTIFICATE-----\n"),
            uuid: uuid.into(),
        }
    }

    fn on_account(machines: &[&str]) -> Membership {
        Membership::Machines(
            machines
                .iter()
                .map(|m| m.to_string())
                .collect::<HashSet<_>>(),
        )
    }

    /// A host whose activity is a script, and whose restarts are counted
    /// along with what the state file held at the moment of each.
    #[derive(Default)]
    struct Fake {
        script: StdMutex<VecDeque<Option<bool>>>,
        polls: AtomicUsize,
        restarts: StdMutex<Vec<(usize, Vec<String>)>>,
        file: Option<PathBuf>,
        /// Run on each poll, to stand in for Sunshine rewriting the file.
        on_poll: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl Fake {
        fn new(script: &[Option<bool>], file: &Path) -> Self {
            Self {
                script: StdMutex::new(script.iter().copied().collect()),
                file: Some(file.to_path_buf()),
                ..Default::default()
            }
        }
        fn restarts(&self) -> Vec<(usize, Vec<String>)> {
            self.restarts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Host for Fake {
        async fn hosting(&self) -> Option<bool> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            if let Some(f) = &self.on_poll {
                f();
            }
            let mut script = self.script.lock().unwrap();
            // The last answer holds once the script runs out.
            if script.len() > 1 {
                script.pop_front().unwrap()
            } else {
                script.front().copied().flatten()
            }
        }
        async fn restart(&self) -> Result<(), String> {
            let held = self.file.as_deref().map(uuids).unwrap_or_default();
            self.restarts
                .lock()
                .unwrap()
                .push((self.polls.load(Ordering::SeqCst), held));
            Ok(())
        }
    }

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

    #[tokio::test]
    async fn nothing_restarts_while_the_host_is_hosting() {
        let dir = Dir::new("reconcile-hosting");
        let host = Fake::new(
            &[Some(true), Some(true), Some(true), Some(false)],
            &dir.state(),
        );
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let mut pending = false;
        for round in 1..=3 {
            let out = pass(&mut pending, &dir.state(), &add, &remove, &host)
                .await
                .unwrap();
            assert!(out.pending && !out.restarted, "round {round}");
            assert!(host.restarts().is_empty());
        }
        let out = pass(&mut pending, &dir.state(), &add, &remove, &host)
            .await
            .unwrap();
        assert!(out.restarted && !out.pending);
        assert_eq!(host.restarts().len(), 1);
        assert_eq!(host.restarts()[0].0, 4);

        // Once loaded, a pass with nothing to change restarts nothing.
        let out = pass(&mut pending, &dir.state(), &add, &remove, &host)
            .await
            .unwrap();
        assert!(!out.pending && !out.restarted);
        assert_eq!(host.restarts().len(), 1);
    }

    #[tokio::test]
    async fn a_restart_re_merges_from_the_file_on_disk() {
        let dir = Dir::new("reconcile-remerge");
        let path = dir.state();
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let mut pending = false;

        let busy = Fake::new(&[Some(true)], &path);
        pass(&mut pending, &path, &add, &remove, &busy)
            .await
            .unwrap();
        assert_eq!(uuids(&path), ["U1", "D1"]);

        // Sunshine rewrites the file from what it holds in memory, which
        // never had Dusk's entry. It happens here mid-probe, after this
        // pass has already looked at the file once.
        let rewrite = path.clone();
        let idle = Fake {
            on_poll: Some(Box::new(move || std::fs::write(&rewrite, STATE).unwrap())),
            ..Fake::new(&[Some(false)], &path)
        };
        let out = pass(&mut pending, &path, &add, &remove, &idle)
            .await
            .unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1", "D1"]);
    }

    #[tokio::test]
    async fn revocation_waits_for_hosting_to_end_like_pairing_does() {
        let dir = Dir::new("reconcile-revoke");
        let path = dir.state();
        let paired = [pairing("aa", "D1")];
        let mut pending = false;
        let (add, remove) = plan(&paired, &on_account(&["aa"]));
        let idle = Fake::new(&[Some(false)], &path);
        pass(&mut pending, &path, &add, &remove, &idle)
            .await
            .unwrap();
        assert_eq!(idle.restarts().len(), 1);

        let (add, remove) = plan(&paired, &on_account(&[]));
        let host = Fake::new(&[Some(true), Some(true), Some(false)], &path);
        for _ in 0..2 {
            let out = pass(&mut pending, &path, &add, &remove, &host)
                .await
                .unwrap();
            assert!(out.pending && !out.restarted);
        }
        // Out of the file at once; out of the running host only when idle.
        assert_eq!(uuids(&path), ["U1"]);
        assert!(host.restarts().is_empty());
        pass(&mut pending, &path, &add, &remove, &host)
            .await
            .unwrap();
        assert_eq!(host.restarts(), [(3, vec!["U1".to_string()])]);
    }

    #[tokio::test]
    async fn an_unreachable_host_is_not_treated_as_idle() {
        let dir = Dir::new("reconcile-unreachable");
        let host = Fake::new(&[None], &dir.state());
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let mut pending = false;
        for _ in 0..3 {
            let out = pass(&mut pending, &dir.state(), &add, &remove, &host)
                .await
                .unwrap();
            assert!(out.pending && !out.restarted);
        }
        assert!(host.restarts().is_empty());
    }

    #[tokio::test]
    async fn a_pending_restart_is_retried_on_the_short_poll_not_the_tick() {
        let nudged = Arc::new(Notify::new());
        let passes = Arc::new(AtomicUsize::new(0));
        let timing = Timing {
            tick: Duration::from_secs(3600),
            poll: Duration::from_millis(5),
            ..Timing::REAL
        };
        let counted = passes.clone();
        let looping = nudged.clone();
        let task = tokio::spawn(async move {
            drive(&looping, &timing, move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { true })
            })
            .await
        });
        nudged.notify_one();
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();
        assert!(passes.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn a_host_is_ready_once_it_answers_again() {
        let dir = Dir::new("reconcile-ready");
        let timing = Timing {
            settle: Duration::ZERO,
            retry: Duration::from_millis(1),
            back_within: Duration::from_millis(200),
            ..Timing::REAL
        };
        let returning = Fake::new(&[None, None, Some(false)], &dir.state());
        assert!(back_within(&returning, &timing).await);
        let gone = Fake::new(&[None], &dir.state());
        assert!(!back_within(&gone, &timing).await);
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

//! Keeping the host's list of paired machines in line with your account.
//!
//! The host only reads that list when it starts, and rewrites it from memory
//! whenever it pairs anything itself — dropping entries it never loaded. So
//! the list is kept right by a reconciler rather than a one-off write: it
//! re-merges what Dusk owes the file on a timer and on every change to the
//! account, and restarts the host to load it, but only while nothing is
//! being streamed. Every write is a fresh read and merge, never bytes
//! worked out earlier, because the file may have been rewritten since.
//!
//! A restart that is owed is written down in Dusk's settings, not held in
//! memory, so a Dusk that quits before the stream ends still owes it on the
//! next launch. And a pairing being taken out is only forgotten once the
//! host has loaded the file without it: until then the host can still write
//! it back.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, Notify};

use super::host::plan;
use crate::account::Membership;
use crate::model::{Activity, DEFAULT_HTTP_PORT};
use crate::state::AppState;
use crate::store::Store;
use crate::sunshine::state_file::{self, NamedDevice, StateFileError};
use crate::sunshine::{api, Credentials, Known, SunshineApi};

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
pub(super) struct Local<'a> {
    pub http: &'a reqwest::Client,
    pub credentials: &'a Credentials,
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

/// Where the host keeps the files this needs.
pub(super) struct Files {
    pub state: PathBuf,
    pub server_cert: PathBuf,
}

/// Paths as the host is configured, or its defaults when the config cannot
/// be read: an unset key is simply absent from it.
pub(super) async fn files(credentials: &Credentials) -> Option<Files> {
    let config = match SunshineApi::new(api::DEFAULT_PORT) {
        Ok(api) => api.get_config(credentials).await.ok(),
        Err(_) => None,
    };
    Some(Files {
        state: state_file::state_path(config.as_ref())?,
        server_cert: state_file::server_cert_path(config.as_ref())?,
    })
}

/// Whether the host owes a restart, wherever that is kept.
#[async_trait]
trait Owed: Send + Sync {
    async fn get(&self) -> bool;
    async fn set(&self, owed: bool);
}

/// Kept in Dusk's settings file, so it outlives this run.
struct Settings<'a> {
    store: &'a Mutex<Store>,
    path: &'a Path,
}

#[async_trait]
impl Owed for Settings<'_> {
    async fn get(&self) -> bool {
        self.store.lock().await.restart_owed
    }

    async fn set(&self, owed: bool) {
        let mut store = self.store.lock().await;
        if store.restart_owed == owed {
            return;
        }
        store.restart_owed = owed;
        if let Err(err) = store.save(self.path) {
            eprintln!("dusk: could not note whether hosting needs a restart: {err}");
        }
    }
}

/// How one pass went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// The file holds something the running host has not loaded yet.
    pub pending: bool,
    /// This pass restarted the host.
    pub restarted: bool,
}

/// Bring the file in line with `add` and `remove`, and restart the host if
/// it is owed one and nothing is streaming.
async fn pass(
    owed: &dyn Owed,
    path: &Path,
    add: &[NamedDevice],
    remove: &[String],
    host: &dyn Host,
) -> Result<Pass, StateFileError> {
    let mut pending = owed.get().await;
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        // Noted before the write rather than after, so no moment exists
        // where the file is changed and nothing says so.
        owed.set(true).await;
        if let Err(err) = state_file::write_atomic(path, &merged) {
            owed.set(pending).await;
            return Err(err);
        }
        pending = true;
    }
    let waiting = Pass {
        pending,
        restarted: false,
    };
    if !pending || host.hosting().await != Some(false) {
        return Ok(waiting);
    }
    // Once more, from disk: the host may have rewritten the file while it
    // was being asked, and a restart loads whatever is there now.
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        state_file::write_atomic(path, &merged)?;
    }
    match host.restart().await {
        Ok(()) => {
            owed.set(false).await;
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
pub(super) struct Timing {
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
    pub(super) const REAL: Timing = Timing {
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

/// Whether a machine just paired can stream from here now: the host has
/// nothing left to load, or it has just restarted and is answering again.
pub(super) async fn ready(outcome: Pass, host: &dyn Host, timing: &Timing) -> bool {
    if outcome.restarted {
        back_within(host, timing).await
    } else {
        !outcome.pending
    }
}

/// Keeps the host's list of paired machines in line with your account.
pub struct Reconciler {
    nudged: Arc<Notify>,
    /// One pass at a time. Two read, merge, write cycles interleaving could
    /// each write a file missing the other's entry.
    running: Mutex<()>,
}

impl Reconciler {
    /// `nudged` is shared with whatever should be able to wake it.
    pub fn new(nudged: Arc<Notify>) -> Self {
        Self {
            nudged,
            running: Mutex::new(()),
        }
    }

    /// Run a pass now rather than at the next tick.
    pub fn nudge(&self) {
        self.nudged.notify_one();
    }

    /// Run passes in the background for as long as Dusk runs.
    ///
    /// The first pass waits for the nudge sent once the keystore has been
    /// read at startup: before then nothing is known. That pass is what puts
    /// the list right after the host was reinstalled, rewrote its file, or
    /// was still owed a restart when Dusk last quit.
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
    ///
    /// Reads nothing from the keystore: a background pass must never be
    /// what puts a keystore prompt on screen.
    async fn background(&self, state: &AppState) -> bool {
        let credentials = match state.credentials.peek().await {
            Known::SignedIn => state.credentials.load().await,
            _ => None,
        };
        let Some(credentials) = credentials else {
            return state.store.lock().await.restart_owed;
        };
        let membership = state.account.known_membership().await;
        let host = Local {
            http: &state.http,
            credentials: &credentials,
        };
        let path = async { files(&credentials).await.map(|f| f.state) };
        match self
            .run(&state.store, &state.store_path, &membership, path, &host)
            .await
        {
            Ok(pass) => pass.pending,
            Err(err) => {
                eprintln!("dusk: could not update this machine's paired machines: {err}");
                state.store.lock().await.restart_owed
            }
        }
    }

    /// Plan from what is stored now, apply it to the file at `state_path`
    /// (looked up only if there is anything to do), and forget pairings
    /// once the host has loaded a file without them.
    pub(super) async fn run(
        &self,
        store: &Mutex<Store>,
        store_path: &Path,
        membership: &Membership,
        state_path: impl Future<Output = Option<PathBuf>>,
        host: &dyn Host,
    ) -> Result<Pass, StateFileError> {
        let _one = self.running.lock().await;
        let (pairings, retired, owed) = {
            let store = store.lock().await;
            (
                store.account_pairings.clone(),
                store.retired_pairings.clone(),
                store.restart_owed,
            )
        };
        if pairings.is_empty() && retired.is_empty() && !owed {
            return Ok(Pass::default());
        }
        let (add, mut remove) = plan(&pairings, membership);
        remove.extend(retired);
        let path = state_path.await.ok_or(StateFileError::Missing)?;
        let settings = Settings {
            store,
            path: store_path,
        };
        let outcome = pass(&settings, &path, &add, &remove, host).await?;
        // Not while a restart is owed: the running host still has them, and
        // its next rewrite would put them back in the file with nothing left
        // here to take them out again.
        if !outcome.pending && !remove.is_empty() {
            let mut store = store.lock().await;
            store.account_pairings.retain(|p| !remove.contains(&p.uuid));
            store.retired_pairings.retain(|u| !remove.contains(u));
            if let Err(err) = store.save(store_path) {
                eprintln!("dusk: could not save paired machines: {err}");
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{on_account, pairing, uuids, Dir, Fake, STATE};
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Owed held in memory, for driving `pass` alone.
    #[derive(Default)]
    struct Flag(AtomicBool);

    #[async_trait]
    impl Owed for Flag {
        async fn get(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        async fn set(&self, owed: bool) {
            self.0.store(owed, Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn nothing_restarts_while_the_host_is_hosting() {
        let dir = Dir::new("reconcile-hosting");
        let host = Fake::new(
            &[Some(true), Some(true), Some(true), Some(false)],
            &dir.state(),
        );
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let owed = Flag::default();
        for round in 1..=3 {
            let out = pass(&owed, &dir.state(), &add, &remove, &host)
                .await
                .unwrap();
            assert!(out.pending && !out.restarted, "round {round}");
            assert!(host.restarts().is_empty());
        }
        let out = pass(&owed, &dir.state(), &add, &remove, &host)
            .await
            .unwrap();
        assert!(out.restarted && !out.pending);
        assert_eq!(host.restarts().len(), 1);
        assert_eq!(host.restarts()[0].0, 4);

        // Once loaded, a pass with nothing to change restarts nothing.
        let out = pass(&owed, &dir.state(), &add, &remove, &host)
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
        let owed = Flag::default();

        let busy = Fake::new(&[Some(true)], &path);
        pass(&owed, &path, &add, &remove, &busy).await.unwrap();
        assert_eq!(uuids(&path), ["U1", "D1"]);

        // The host rewrites the file from what it holds in memory, which
        // never had Dusk's entry. It happens here mid-probe, after this
        // pass has already looked at the file once.
        let rewrite = path.clone();
        let idle = Fake {
            on_poll: Some(Box::new(move || std::fs::write(&rewrite, STATE).unwrap())),
            ..Fake::new(&[Some(false)], &path)
        };
        let out = pass(&owed, &path, &add, &remove, &idle).await.unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1", "D1"]);
    }

    #[tokio::test]
    async fn revocation_waits_for_hosting_to_end_like_pairing_does() {
        let dir = Dir::new("reconcile-revoke");
        let path = dir.state();
        let paired = [pairing("aa", "D1")];
        let owed = Flag::default();
        let (add, remove) = plan(&paired, &on_account(&["aa"]));
        let idle = Fake::new(&[Some(false)], &path);
        pass(&owed, &path, &add, &remove, &idle).await.unwrap();
        assert_eq!(idle.restarts().len(), 1);

        let (add, remove) = plan(&paired, &on_account(&[]));
        let host = Fake::new(&[Some(true), Some(true), Some(false)], &path);
        for _ in 0..2 {
            let out = pass(&owed, &path, &add, &remove, &host).await.unwrap();
            assert!(out.pending && !out.restarted);
        }
        // Out of the file at once; out of the running host only when idle.
        assert_eq!(uuids(&path), ["U1"]);
        assert!(host.restarts().is_empty());
        pass(&owed, &path, &add, &remove, &host).await.unwrap();
        assert_eq!(host.restarts(), [(3, vec!["U1".to_string()])]);
    }

    #[tokio::test]
    async fn an_unreachable_host_is_not_treated_as_idle() {
        let dir = Dir::new("reconcile-unreachable");
        let host = Fake::new(&[None], &dir.state());
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let owed = Flag::default();
        for _ in 0..3 {
            let out = pass(&owed, &dir.state(), &add, &remove, &host)
                .await
                .unwrap();
            assert!(out.pending && !out.restarted);
        }
        assert!(host.restarts().is_empty());
    }

    fn stored(pairings: &[crate::store::AccountPairing]) -> Mutex<Store> {
        Mutex::new(Store {
            account_pairings: pairings.to_vec(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn a_revoked_pairing_is_kept_until_the_host_has_loaded_its_removal() {
        let dir = Dir::new("reconcile-revoke-kept");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let reconciler = Reconciler::new(Arc::default());
        let at = || async { Some(dir.state()) };

        let idle = Fake::new(&[Some(false)], &path);
        let paired = reconciler
            .run(&store, &dir.store(), &on_account(&["aa"]), at(), &idle)
            .await
            .unwrap();
        assert!(paired.restarted);
        let with_d1 = std::fs::read_to_string(&path).unwrap();

        // Taken off the account mid-stream: out of the file, still stored.
        let busy = Fake::new(&[Some(true)], &path);
        let off = on_account(&[]);
        let out = reconciler
            .run(&store, &dir.store(), &off, at(), &busy)
            .await
            .unwrap();
        assert!(out.pending);
        assert_eq!(uuids(&path), ["U1"]);
        assert_eq!(store.lock().await.account_pairings.len(), 1);

        // The host pairs something by PIN and rewrites the file from
        // memory, which still has D1.
        std::fs::write(&path, &with_d1).unwrap();
        let idle = Fake::new(&[Some(false)], &path);
        let out = reconciler
            .run(&store, &dir.store(), &off, at(), &idle)
            .await
            .unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1"]);
        assert!(store.lock().await.account_pairings.is_empty());
    }

    #[tokio::test]
    async fn a_restart_owed_when_dusk_quit_is_still_owed_after_relaunch() {
        let dir = Dir::new("reconcile-relaunch");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let busy = Fake::new(&[Some(true)], &path);
        let out = Reconciler::new(Arc::default())
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(dir.state()) },
                &busy,
            )
            .await
            .unwrap();
        assert!(out.pending);
        assert!(!ready(out, &busy, &Timing::REAL).await);

        // Dusk quits. Next launch reads its settings back from disk; the
        // file is already current, so only the note says a restart is owed.
        let relaunched = Mutex::new(Store::load(&dir.store()));
        assert!(relaunched.lock().await.restart_owed);
        let still_busy = Fake::new(&[Some(true)], &path);
        let out = Reconciler::new(Arc::default())
            .run(
                &relaunched,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(dir.state()) },
                &still_busy,
            )
            .await
            .unwrap();
        assert!(out.pending && !out.restarted);
        assert!(!ready(out, &still_busy, &Timing::REAL).await);

        let idle = Fake::new(&[Some(false)], &path);
        let out = Reconciler::new(Arc::default())
            .run(
                &relaunched,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(dir.state()) },
                &idle,
            )
            .await
            .unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1", "D1"]);
        assert!(!Store::load(&dir.store()).restart_owed);
    }

    #[tokio::test]
    async fn nothing_stored_and_nothing_owed_does_not_look_for_the_file() {
        let reconciler = Reconciler::new(Arc::default());
        let store = Mutex::new(Store::default());
        let host = Fake::default();
        let out = reconciler
            .run(
                &store,
                Path::new("unused.json"),
                &Membership::SignedOut,
                async { None::<PathBuf>.or_else(|| panic!("looked for the file")) },
                &host,
            )
            .await
            .unwrap();
        assert_eq!(out, Pass::default());
        assert_eq!(host.polls.load(Ordering::SeqCst), 0);
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
        let restarted = Pass {
            pending: false,
            restarted: true,
        };
        let returning = Fake::new(&[None, None, Some(false)], &dir.state());
        assert!(ready(restarted, &returning, &timing).await);
        let gone = Fake::new(&[None], &dir.state());
        assert!(!ready(restarted, &gone, &timing).await);
        // Nothing was owed: ready without asking.
        let quiet = Fake::new(&[None], &dir.state());
        assert!(ready(Pass::default(), &quiet, &timing).await);
        assert_eq!(quiet.polls.load(Ordering::SeqCst), 0);
    }
}

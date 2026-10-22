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
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// Ask the host to restart. `Ok` means it took the request, not that it
    /// has restarted: see [`came_back`] for that.
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
        if !matches!(info.activity(), Activity::Hosting { .. }) {
            return Some(false);
        }
        // The host stays busy for as long as the app a stream launched is
        // running, which outlasts a client that dropped instead of quitting
        // — for good, if nobody comes back to it. Only its log says whether
        // a client is still there. A log that cannot be read keeps it busy.
        let log = match SunshineApi::new(api::DEFAULT_PORT) {
            Ok(api) => api.get_logs(self.credentials).await.ok(),
            Err(_) => None,
        };
        Some(log.map_or(true, |log| client_connected(&log)))
    }

    async fn restart(&self) -> Result<(), String> {
        SunshineApi::new(api::DEFAULT_PORT)
            .map_err(|e| e.to_string())?
            .restart_checked(self.credentials)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Whether the host's log leaves a client connected. A session starts with
/// `New streaming session started` and ends when its client disconnects or
/// stops answering pings. It counts as over only when the last of those is
/// an end and there have been at least as many ends as starts, so a second
/// client still streaming keeps it busy. A log with none of them says
/// nothing, and is taken as busy.
fn client_connected(log: &str) -> bool {
    let (mut starts, mut ends, mut last_ended) = (0usize, 0usize, None);
    for line in log.lines() {
        if line.contains("New streaming session started") {
            starts += 1;
            last_ended = Some(false);
        } else if line.contains("CLIENT CONNECTED") {
            last_ended = Some(false);
        } else if line.contains("CLIENT DISCONNECTED") || line.contains("Ping Timeout") {
            ends += 1;
            last_ended = Some(true);
        }
    }
    !(last_ended == Some(true) && ends >= starts)
}

/// Whether this machine's host has a session open that no client is
/// watching: the app a stream launched is still running and its client
/// dropped instead of quitting. The next stream resumes that session, and a
/// resumed one runs none of the host's prep commands. Anything unreadable
/// answers no.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) async fn session_left_open(http: &reqwest::Client, credentials: &Credentials) -> bool {
    let Ok(info) = crate::serverinfo::query_http(http, "127.0.0.1", DEFAULT_HTTP_PORT).await else {
        return false;
    };
    if !matches!(info.activity(), Activity::Hosting { .. }) {
        return false;
    }
    let log = match SunshineApi::new(api::DEFAULT_PORT) {
        Ok(api) => api.get_logs(credentials).await.ok(),
        Err(_) => None,
    };
    log.is_some_and(|log| !client_connected(&log))
}

/// Where the host keeps the files this needs.
pub(super) struct Files {
    pub state: PathBuf,
    pub server_cert: PathBuf,
}

/// Paths as the host is configured: an unset key is simply absent from it,
/// and means the default. `None` when the config cannot be read, never the
/// defaults: a host configured elsewhere would be answered with a
/// certificate it does not use, or have a file it does not read changed.
pub(super) async fn files(credentials: &Credentials) -> Option<Files> {
    let config = match SunshineApi::new(api::DEFAULT_PORT) {
        Ok(api) => api.get_config(credentials).await.ok(),
        Err(_) => None,
    };
    located(config.as_ref())
}

fn located(config: Option<&serde_json::Map<String, serde_json::Value>>) -> Option<Files> {
    let config = Some(config?);
    Some(Files {
        state: state_file::state_path(config)?,
        server_cert: state_file::server_cert_path(config)?,
    })
}

/// Whether the host owes a restart, wherever that is kept.
#[async_trait]
trait Owed: Send + Sync {
    async fn get(&self) -> bool;
    /// An error means the note was not kept, and nothing changed.
    async fn set(&self, owed: bool) -> Result<(), String>;
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

    async fn set(&self, owed: bool) -> Result<(), String> {
        let mut store = self.store.lock().await;
        if store.restart_owed == owed {
            return Ok(());
        }
        store.restart_owed = owed;
        if let Err(err) = store.save(self.path) {
            // Put back, so memory never claims what the disk does not.
            store.restart_owed = !owed;
            return Err(err);
        }
        Ok(())
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

/// Restarts in a row that were not seen to happen before Dusk stops asking
/// on every pass and starts waiting between tries.
const UNCONFIRMED_LIMIT: u32 = 3;

/// The longest wait between tries at a restart that keeps not happening.
const BACK_OFF_CAP: Duration = Duration::from_secs(60 * 60);

/// Restarts asked for in a row that were not seen to happen, and when the
/// next may be asked for. Held in memory only: it limits how often a host
/// is asked, and nothing else. The restart stays owed, and nothing waiting
/// on it is forgotten, however long this waits.
#[derive(Debug, Default)]
pub(super) struct Unconfirmed {
    count: u32,
    not_before: Option<Instant>,
    /// Why the last one did not happen, for the window: an owed restart
    /// that keeps not happening is otherwise invisible, and every machine
    /// that pairs with this one is told "try again in a minute" for as
    /// long as it goes on.
    reason: Option<String>,
}

impl Unconfirmed {
    fn allows(&self, now: Instant) -> bool {
        self.count < UNCONFIRMED_LIMIT || self.not_before.map_or(true, |at| now >= at)
    }

    /// One more that did not happen. From the limit on, the wait starts at
    /// the tick and doubles with each, up to [`BACK_OFF_CAP`].
    fn missed(&mut self, now: Instant, timing: &Timing, reason: String) {
        self.count = self.count.saturating_add(1);
        self.reason = Some(reason);
        if self.count >= UNCONFIRMED_LIMIT {
            let doublings = (self.count - UNCONFIRMED_LIMIT).min(16);
            let wait = timing.tick.saturating_mul(1 << doublings).min(BACK_OFF_CAP);
            self.not_before = Some(now + wait);
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

fn lock(misses: &std::sync::Mutex<Unconfirmed>) -> std::sync::MutexGuard<'_, Unconfirmed> {
    misses.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bring the file in line with `add` and `remove`, and restart the host if
/// it is owed one and nothing is streaming, and it has not just failed to
/// restart too often (see [`Unconfirmed`]). The restart is only counted
/// once it has been seen to happen, and only then is it no longer owed.
async fn pass(
    owed: &dyn Owed,
    path: &Path,
    add: &[NamedDevice],
    remove: &[String],
    host: &dyn Host,
    timing: &Timing,
    misses: &std::sync::Mutex<Unconfirmed>,
) -> Result<Pass, StateFileError> {
    let mut pending = owed.get().await;
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        // Noted before the write rather than after, so no moment exists
        // where the file is changed and nothing says so. A note that cannot
        // be kept stops the write: a restart owed and forgotten would leave
        // an addition unloaded, or a removal pruned while the host holds it.
        owed.set(true).await.map_err(|err| {
            StateFileError::Unwritable(format!("could not note the restart it needs: {err}"))
        })?;
        if let Err(err) = state_file::write_atomic(path, &merged) {
            if let Err(undo) = owed.set(pending).await {
                eprintln!("dusk: could not note whether hosting needs a restart: {undo}");
            }
            return Err(err);
        }
        pending = true;
        // Something new for the host to load: worth asking again at once.
        lock(misses).reset();
    }
    let waiting = Pass {
        pending,
        restarted: false,
    };
    if !pending || !lock(misses).allows(Instant::now()) {
        return Ok(waiting);
    }
    if host.hosting().await != Some(false) {
        return Ok(waiting);
    }
    // Once more, from disk: the host may have rewritten the file while it
    // was being asked, and a restart loads whatever is there now.
    if let Some(merged) = state_file::merge(&state_file::read(path)?, add, remove)? {
        state_file::write_atomic(path, &merged)?;
    }
    if let Err(err) = host.restart().await {
        eprintln!("dusk: could not restart hosting to load paired machines: {err}");
        lock(misses).missed(Instant::now(), timing, err);
        return Ok(waiting);
    }
    match came_back(host, timing).await {
        Observed::CameBack => {}
        // Still owed, so a later pass asks again: the next one, until a few
        // in a row have gone this way, and then less and less often.
        Observed::NeverWentDown => {
            let reason = "hosting took the restart request but did not restart".to_string();
            eprintln!("dusk: {reason}; it is still owed");
            lock(misses).missed(Instant::now(), timing, reason);
            return Ok(waiting);
        }
        Observed::StillDown => {
            let reason = format!(
                "hosting went down for the restart but had not come back after {:?}",
                timing.back_within.saturating_mul(1 + STILL_DOWN_PATIENCE)
            );
            eprintln!("dusk: {reason}; the restart is still owed");
            lock(misses).missed(Instant::now(), timing, reason);
            return Ok(waiting);
        }
    }
    lock(misses).reset();
    // Forgetting to clear it costs one needless restart, no more.
    if let Err(err) = owed.set(false).await {
        eprintln!("dusk: could not note that hosting has restarted: {err}");
    }
    Ok(Pass {
        pending: false,
        restarted: true,
    })
}

/// What watching a restart showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    /// Seen not answering, then answering again: the new process is up.
    CameBack,
    /// Never seen not answering: the request was taken and ignored, or the
    /// old process is still on its way out. Either way what answers still
    /// holds the list as it was.
    NeverWentDown,
    /// Seen not answering, and still not answering when the watching
    /// stopped: a slow start, or one that failed.
    StillDown,
}

/// How many more `back_within`s a host seen going down is given to come
/// back, on top of the first. A host that is down is restarting, or has
/// failed to; asking it again does nothing either way, and asking a slow
/// one again cost a second restart, right after the first.
const STILL_DOWN_PATIENCE: u32 = 2;

/// Whether a restart the host was asked for really happened: it is seen not
/// answering at least once, and then answering again, within `back_within`
/// — or, once seen down, within a few `back_within`s more.
///
/// An answer before it has gone down is the process on its way out — or one
/// that never left — and counts for nothing: that one still holds the list
/// as it was. Asked every `probe`, which is far shorter than the time the
/// host spends down: it stops serving before it exits and only serves again
/// once the new process has loaded its files.
async fn came_back(host: &dyn Host, timing: &Timing) -> Observed {
    let gone = std::sync::atomic::AtomicBool::new(false);
    let watch = async {
        loop {
            match host.hosting().await {
                None => gone.store(true, Ordering::SeqCst),
                Some(_) if gone.load(Ordering::SeqCst) => return,
                Some(_) => {}
            }
            tokio::time::sleep(timing.probe).await;
        }
    };
    tokio::pin!(watch);
    if tokio::time::timeout(timing.back_within, &mut watch)
        .await
        .is_ok()
    {
        return Observed::CameBack;
    }
    if !gone.load(Ordering::SeqCst) {
        return Observed::NeverWentDown;
    }
    let more = timing.back_within.saturating_mul(STILL_DOWN_PATIENCE);
    match tokio::time::timeout(more, &mut watch).await {
        Ok(()) => Observed::CameBack,
        Err(_) => Observed::StillDown,
    }
}

/// How long the reconciler waits for things.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    /// Between passes when nothing is owed.
    tick: Duration,
    /// Between passes while a restart is owed.
    poll: Duration,
    /// Between asks while watching a restart: short enough that the time
    /// the host spends down cannot fall between two of them.
    probe: Duration,
    /// How long a restart has to be seen going down and coming back.
    back_within: Duration,
}

impl Timing {
    pub(super) const REAL: Timing = Timing {
        tick: Duration::from_secs(60),
        poll: Duration::from_secs(15),
        probe: Duration::from_millis(250),
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

/// Whether a machine just paired can stream from here now: the host has
/// nothing left to load. A pass that restarted it has already seen it go
/// down and come back (see [`came_back`]); one where that was not seen is
/// still pending, so an answer from the old process never makes it ready.
pub(super) fn ready(outcome: Pass) -> bool {
    !outcome.pending
}

/// Keeps the host's list of paired machines in line with your account.
pub struct Reconciler {
    nudged: Arc<Notify>,
    /// One pass at a time. Two read, merge, write cycles interleaving could
    /// each write a file missing the other's entry.
    running: Mutex<()>,
    unconfirmed: std::sync::Mutex<Unconfirmed>,
}

impl Reconciler {
    /// `nudged` is shared with whatever should be able to wake it.
    pub fn new(nudged: Arc<Notify>) -> Self {
        Self {
            nudged,
            running: Mutex::new(()),
            unconfirmed: Default::default(),
        }
    }

    /// Run a pass now rather than at the next tick.
    pub fn nudge(&self) {
        self.nudged.notify_one();
    }

    /// Why the restart the host owes keeps not happening, if it does, in
    /// words for the window. `None` while nothing has gone wrong.
    pub fn trouble(&self) -> Option<String> {
        lock(&self.unconfirmed).reason.clone()
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
        // Nothing at all when the config cannot be read, never the default
        // paths: a host configured elsewhere would have a file it does not
        // read changed, and a removal written there and then forgotten here
        // would stay in the one it does. With no path the pass writes
        // nothing, forgets nothing, and whatever restart is owed stays owed.
        let path = async { files(&credentials).await.map(|f| f.state) };
        match self
            .run(
                &state.store,
                &state.store_path,
                &membership,
                path,
                &host,
                &Timing::REAL,
            )
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
        timing: &Timing,
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
        let outcome = pass(
            &settings,
            &path,
            &add,
            &remove,
            host,
            timing,
            &self.unconfirmed,
        )
        .await?;
        // Not while a restart is owed, which includes one asked for but not
        // seen to happen: the running host still has them, and its next
        // rewrite would put them back in the file with nothing left here to
        // take them out again.
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

    /// Watching a restart without waiting on a real one.
    const QUICK: Timing = Timing {
        probe: Duration::from_millis(1),
        back_within: Duration::from_millis(200),
        ..Timing::REAL
    };

    /// Owed held in memory, for driving `pass` alone.
    #[derive(Default)]
    struct Flag(AtomicBool);

    #[async_trait]
    impl Owed for Flag {
        async fn get(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        async fn set(&self, owed: bool) -> Result<(), String> {
            self.0.store(owed, Ordering::SeqCst);
            Ok(())
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
            let out = pass(
                &owed,
                &dir.state(),
                &add,
                &remove,
                &host,
                &QUICK,
                &Default::default(),
            )
            .await
            .unwrap();
            assert!(out.pending && !out.restarted, "round {round}");
            assert!(host.restarts().is_empty());
        }
        let out = pass(
            &owed,
            &dir.state(),
            &add,
            &remove,
            &host,
            &QUICK,
            &Default::default(),
        )
        .await
        .unwrap();
        assert!(out.restarted && !out.pending);
        assert_eq!(host.restarts().len(), 1);
        assert_eq!(host.restarts()[0].0, 4);

        // Once loaded, a pass with nothing to change restarts nothing.
        let out = pass(
            &owed,
            &dir.state(),
            &add,
            &remove,
            &host,
            &QUICK,
            &Default::default(),
        )
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
        pass(
            &owed,
            &path,
            &add,
            &remove,
            &busy,
            &QUICK,
            &Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(uuids(&path), ["U1", "D1"]);

        // The host rewrites the file from what it holds in memory, which
        // never had Dusk's entry. It happens here mid-probe, after this
        // pass has already looked at the file once.
        let rewrite = path.clone();
        let idle = Fake {
            on_poll: Some(Box::new(move || std::fs::write(&rewrite, STATE).unwrap())),
            ..Fake::new(&[Some(false)], &path)
        };
        let out = pass(
            &owed,
            &path,
            &add,
            &remove,
            &idle,
            &QUICK,
            &Default::default(),
        )
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
        let owed = Flag::default();
        let (add, remove) = plan(&paired, &on_account(&["aa"]));
        let idle = Fake::new(&[Some(false)], &path);
        pass(
            &owed,
            &path,
            &add,
            &remove,
            &idle,
            &QUICK,
            &Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(idle.restarts().len(), 1);

        let (add, remove) = plan(&paired, &on_account(&[]));
        let host = Fake::new(&[Some(true), Some(true), Some(false)], &path);
        for _ in 0..2 {
            let out = pass(
                &owed,
                &path,
                &add,
                &remove,
                &host,
                &QUICK,
                &Default::default(),
            )
            .await
            .unwrap();
            assert!(out.pending && !out.restarted);
        }
        // Out of the file at once; out of the running host only when idle.
        assert_eq!(uuids(&path), ["U1"]);
        assert!(host.restarts().is_empty());
        pass(
            &owed,
            &path,
            &add,
            &remove,
            &host,
            &QUICK,
            &Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(host.restarts(), [(3, vec!["U1".to_string()])]);
    }

    #[tokio::test]
    async fn an_unreachable_host_is_not_treated_as_idle() {
        let dir = Dir::new("reconcile-unreachable");
        let host = Fake::new(&[None], &dir.state());
        let (add, remove) = plan(&[pairing("aa", "D1")], &on_account(&["aa"]));
        let owed = Flag::default();
        for _ in 0..3 {
            let out = pass(
                &owed,
                &dir.state(),
                &add,
                &remove,
                &host,
                &QUICK,
                &Default::default(),
            )
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
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                at(),
                &idle,
                &QUICK,
            )
            .await
            .unwrap();
        assert!(paired.restarted);
        let with_d1 = std::fs::read_to_string(&path).unwrap();

        // Taken off the account mid-stream: out of the file, still stored.
        let busy = Fake::new(&[Some(true)], &path);
        let off = on_account(&[]);
        let out = reconciler
            .run(&store, &dir.store(), &off, at(), &busy, &QUICK)
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
            .run(&store, &dir.store(), &off, at(), &idle, &QUICK)
            .await
            .unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1"]);
        assert!(store.lock().await.account_pairings.is_empty());
    }

    #[tokio::test]
    async fn a_restart_the_host_never_went_down_for_is_still_owed() {
        let dir = Dir::new("reconcile-never-down");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let reconciler = Reconciler::new(Arc::default());
        let at = || async { Some(dir.state()) };
        let idle = Fake::new(&[Some(false)], &path);
        reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                at(),
                &idle,
                &QUICK,
            )
            .await
            .unwrap();

        // Taken off the account. The host takes the restart request but
        // never goes down: what answers is still the old process, holding D1.
        let unmoved = Fake {
            stays_up: true,
            ..Fake::new(&[Some(false)], &path)
        };
        let out = reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&[]),
                at(),
                &unmoved,
                &QUICK,
            )
            .await
            .unwrap();
        assert_eq!(unmoved.restarts().len(), 1);
        assert!(out.pending && !out.restarted);
        assert!(store.lock().await.restart_owed);
        assert!(Store::load(&dir.store()).restart_owed);
        assert_eq!(store.lock().await.account_pairings.len(), 1);
        assert!(!ready(out));
    }

    #[tokio::test]
    async fn a_host_slow_to_come_back_is_not_restarted_twice() {
        // The host went down for the restart and is taking its time coming
        // back. Asking again would restart the new process the moment it
        // is up, right after the first; it is waited for instead.
        let dir = Dir::new("reconcile-slow-back");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let reconciler = Reconciler::new(Arc::default());
        let timing = Timing {
            back_within: Duration::from_millis(150),
            ..QUICK
        };
        let slow = Fake {
            down_for: 200,
            ..Fake::new(&[Some(false)], &path)
        };
        let out = reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(dir.state()) },
                &slow,
                &timing,
            )
            .await
            .unwrap();
        assert!(out.restarted && !out.pending);
        assert_eq!(slow.restarts().len(), 1);
        assert!(!store.lock().await.restart_owed);
        assert!(reconciler.trouble().is_none());
    }

    #[tokio::test]
    async fn an_owed_restart_that_keeps_not_happening_says_why() {
        let dir = Dir::new("reconcile-trouble");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let reconciler = Reconciler::new(Arc::default());
        let at = || async { Some(dir.state()) };
        let unmoved = Fake {
            stays_up: true,
            ..Fake::new(&[Some(false)], &path)
        };
        let out = reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                at(),
                &unmoved,
                &QUICK,
            )
            .await
            .unwrap();
        assert!(out.pending);
        let why = reconciler.trouble().expect("says what went wrong");
        assert!(why.contains("did not restart"), "{why}");

        // A host that does restart clears it.
        let idle = Fake::new(&[Some(false)], &path);
        let out = reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                at(),
                &idle,
                &QUICK,
            )
            .await
            .unwrap();
        assert!(out.restarted);
        assert!(reconciler.trouble().is_none());
    }

    #[tokio::test]
    async fn a_host_that_never_goes_down_is_restarted_three_times_then_less_often() {
        let dir = Dir::new("reconcile-backoff");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        let reconciler = Reconciler::new(Arc::default());
        let at = || async { Some(dir.state()) };
        let timing = Timing {
            tick: Duration::from_millis(300),
            ..QUICK
        };
        let paired = Fake::new(&[Some(false)], &path);
        reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                at(),
                &paired,
                &timing,
            )
            .await
            .unwrap();
        let unmoved = Fake {
            stays_up: true,
            ..Fake::new(&[Some(false)], &path)
        };
        for _ in 0..6 {
            let out = reconciler
                .run(
                    &store,
                    &dir.store(),
                    &on_account(&[]),
                    at(),
                    &unmoved,
                    &timing,
                )
                .await
                .unwrap();
            assert!(out.pending && !out.restarted);
        }
        assert_eq!(unmoved.restarts().len(), 3);
        // Still owed, and the revoked pairing still kept, the whole time.
        assert!(store.lock().await.restart_owed);
        assert_eq!(store.lock().await.account_pairings.len(), 1);

        // Once the back-off has passed, one more try, and then a longer wait.
        tokio::time::sleep(timing.tick).await;
        for _ in 0..3 {
            reconciler
                .run(
                    &store,
                    &dir.store(),
                    &on_account(&[]),
                    at(),
                    &unmoved,
                    &timing,
                )
                .await
                .unwrap();
        }
        assert_eq!(unmoved.restarts().len(), 4);
        tokio::time::sleep(timing.tick).await;
        reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&[]),
                at(),
                &unmoved,
                &timing,
            )
            .await
            .unwrap();
        assert_eq!(
            unmoved.restarts().len(),
            4,
            "the second wait is twice the first"
        );

        // A host that does restart puts it right: confirmed, pruned, and the
        // count starts again.
        let idle = Fake::new(&[Some(false)], &path);
        tokio::time::sleep(timing.tick).await;
        let out = reconciler
            .run(&store, &dir.store(), &on_account(&[]), at(), &idle, &timing)
            .await
            .unwrap();
        assert!(out.restarted);
        assert!(!store.lock().await.restart_owed);
        assert!(store.lock().await.account_pairings.is_empty());
    }

    #[test]
    fn the_back_off_doubles_up_to_an_hour() {
        let mut misses = Unconfirmed::default();
        let now = Instant::now();
        let timing = Timing::REAL;
        for _ in 0..UNCONFIRMED_LIMIT {
            assert!(misses.allows(now));
            misses.missed(now, &timing, String::new());
        }
        assert!(!misses.allows(now));
        assert!(misses.allows(now + Duration::from_secs(60)));
        misses.missed(now, &timing, String::new());
        assert!(!misses.allows(now + Duration::from_secs(119)));
        assert!(misses.allows(now + Duration::from_secs(120)));
        for _ in 0..20 {
            misses.missed(now, &timing, String::new());
        }
        assert!(misses.allows(now + Duration::from_secs(3600)));
        misses.reset();
        assert!(misses.allows(now));
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
                &QUICK,
            )
            .await
            .unwrap();
        assert!(out.pending);
        assert!(!ready(out));

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
                &QUICK,
            )
            .await
            .unwrap();
        assert!(out.pending && !out.restarted);
        assert!(!ready(out));

        let idle = Fake::new(&[Some(false)], &path);
        let out = Reconciler::new(Arc::default())
            .run(
                &relaunched,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(dir.state()) },
                &idle,
                &QUICK,
            )
            .await
            .unwrap();
        assert!(out.restarted);
        assert_eq!(idle.restarts()[0].1, ["U1", "D1"]);
        assert!(!Store::load(&dir.store()).restart_owed);
    }

    #[tokio::test]
    async fn a_restart_it_cannot_note_leaves_the_file_and_the_pairings_alone() {
        let dir = Dir::new("reconcile-unsaved");
        let path = dir.state();
        // Under a regular file, so no settings can be saved there.
        let unsaveable = path.join("devices.json");
        let reconciler = Reconciler::new(Arc::default());
        let idle = Fake::new(&[Some(false)], &path);

        // An addition is not written.
        let store = stored(&[pairing("aa", "D1")]);
        let added = reconciler
            .run(
                &store,
                &unsaveable,
                &on_account(&["aa"]),
                async { Some(path.clone()) },
                &idle,
                &QUICK,
            )
            .await;
        assert!(added.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STATE);
        assert!(!store.lock().await.restart_owed);

        // A removal is not written, and the pairing is not forgotten.
        reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&["aa"]),
                async { Some(path.clone()) },
                &idle,
                &QUICK,
            )
            .await
            .unwrap();
        let with_d1 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(uuids(&path), ["U1", "D1"]);
        let removed = reconciler
            .run(
                &store,
                &unsaveable,
                &on_account(&[]),
                async { Some(path.clone()) },
                &idle,
                &QUICK,
            )
            .await;
        assert!(removed.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), with_d1);
        assert_eq!(store.lock().await.account_pairings.len(), 1);
        assert!(!store.lock().await.restart_owed);
        assert_eq!(idle.restarts().len(), 1);
    }

    #[tokio::test]
    async fn a_pass_that_cannot_find_the_file_writes_and_forgets_nothing() {
        let dir = Dir::new("reconcile-no-paths");
        let path = dir.state();
        let store = stored(&[pairing("aa", "D1")]);
        store.lock().await.restart_owed = true;
        let reconciler = Reconciler::new(Arc::default());
        let host = Fake::new(&[Some(false)], &path);
        let out = reconciler
            .run(
                &store,
                &dir.store(),
                &on_account(&[]),
                async { None },
                &host,
                &QUICK,
            )
            .await;
        assert!(matches!(out, Err(StateFileError::Missing)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STATE);
        assert!(store.lock().await.restart_owed);
        assert_eq!(store.lock().await.account_pairings.len(), 1);
        assert!(host.restarts().is_empty());
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
                &QUICK,
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
    async fn a_restart_counts_once_the_host_is_seen_down_and_back() {
        let dir = Dir::new("reconcile-came-back");
        let returning = Fake::new(&[None, None, Some(false)], &dir.state());
        assert_eq!(came_back(&returning, &QUICK).await, Observed::CameBack);
        // Answering at first is the old process; only what follows counts.
        let late = Fake::new(&[Some(false), Some(false), None, Some(true)], &dir.state());
        assert_eq!(came_back(&late, &QUICK).await, Observed::CameBack);
        // Never down: nothing says it restarted.
        let unmoved = Fake::new(&[Some(false)], &dir.state());
        assert_eq!(came_back(&unmoved, &QUICK).await, Observed::NeverWentDown);
        // Down and never back.
        let gone = Fake::new(&[None], &dir.state());
        assert_eq!(came_back(&gone, &QUICK).await, Observed::StillDown);
    }

    #[test]
    fn a_config_it_cannot_read_gives_no_paths() {
        assert!(located(None).is_none());
        // Absolute on this platform: "/elsewhere" has no drive on Windows.
        let elsewhere = std::env::temp_dir().join("elsewhere");
        let configured = serde_json::json!({
            "file_state": elsewhere.join("state.json"),
            "cert": elsewhere.join("cacert.pem"),
        });
        let at = located(configured.as_object()).unwrap();
        assert_eq!(at.state, elsewhere.join("state.json"));
        assert_eq!(at.server_cert, elsewhere.join("cacert.pem"));
        // A config that was read but leaves them unset means the defaults.
        let unset = serde_json::Map::new();
        let defaults = located(Some(&unset)).unwrap();
        assert_eq!(
            defaults.state,
            state_file::state_path(Some(&unset)).unwrap()
        );
    }

    const STARTED: &str =
        "[2026-10-21 21:00:00.000]: Info: New streaming session started [active sessions: 1]";
    const CONNECTED: &str = "[2026-10-21 21:00:01.000]: Info: CLIENT CONNECTED";
    const DISCONNECTED: &str = "[2026-10-21 21:30:00.000]: Info: CLIENT DISCONNECTED";
    const TIMED_OUT: &str = "[2026-10-21 21:30:00.000]: Info: 192.168.50.146: Ping Timeout";

    #[test]
    fn a_client_that_left_leaves_the_host_free_to_restart() {
        // The app it launched is still running, so serverinfo says busy.
        for end in [DISCONNECTED, TIMED_OUT] {
            let log = [STARTED, CONNECTED, end].join("\n");
            assert!(!client_connected(&log), "{end}");
        }
    }

    #[test]
    fn a_client_still_streaming_keeps_the_host_busy() {
        assert!(client_connected(&[STARTED, CONNECTED].join("\n")));
        // One that left and came back.
        let back = [STARTED, CONNECTED, DISCONNECTED, STARTED, CONNECTED];
        assert!(client_connected(&back.join("\n")));
        // A second client, still there after the first has gone.
        let second = [STARTED, CONNECTED, STARTED, CONNECTED, DISCONNECTED];
        assert!(client_connected(&second.join("\n")));
    }

    #[test]
    fn a_log_that_says_nothing_is_taken_as_busy() {
        assert!(client_connected(""));
        assert!(client_connected(
            "[2026-10-21 21:00:00.000]: Info: Sunshine version: v2026.914.233613"
        ));
    }

    #[test]
    fn a_host_is_ready_only_with_nothing_left_to_load() {
        assert!(ready(Pass::default()));
        assert!(ready(Pass {
            pending: false,
            restarted: true,
        }));
        assert!(!ready(Pass {
            pending: true,
            restarted: false,
        }));
    }
}

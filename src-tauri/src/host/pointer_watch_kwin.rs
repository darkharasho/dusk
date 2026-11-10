//! Loading the window watcher into KWin, and listening to what it says.
//!
//! The Linux half of [`super::pointer_watch`]. Everything here is D-Bus: own
//! a name for the script to call, hand KWin the script, and turn each report
//! into an intent for whoever is streaming.
//!
//! # Why a script inside KWin
//!
//! Wayland gives a third party no way to ask what window has focus, let alone
//! whether it has locked the pointer. KDE is the exception, and only through
//! this door: `org.kde.kwin.Scripting` accepts a script, and a script can
//! call back out with `callDBus`. Every call below was first run by hand with
//! `busctl` against the real compositor, so the shapes here are transcribed
//! rather than guessed — including the two that cost a round trip each
//! (`qdbus` is not installed on Bazzite, and a script's `print()` goes to a
//! logging category that is off by default, so a working script is silent).
//!
//! # Lifetime
//!
//! The script outlives nothing. It is unloaded when the watcher is dropped,
//! and `loadScript` is preceded by an `unloadScript` so a Dusk that died
//! without cleaning up does not leave two copies reporting.

#![cfg(target_os = "linux")]
// Nothing calls this yet, and off Linux the module is empty so there is
// nothing to call: the allow is about where the first caller is, as it is in
// `pointer_watch` and `pointer_intent` beside it. CI caught this rather than
// a local build, because on macOS the `cfg` above leaves no code to be dead.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use zbus::{interface, Connection, Proxy};

use crate::state::AppState;

use super::pointer_intent::PointerIntent;
use super::pointer_watch::{
    intent_of_report, Changes, WatchError, KWIN_IFACE, KWIN_PATH, KWIN_SERVICE, PATH, SCRIPT,
    SERVICE,
};

/// What the script calls into. Owns the change filter, so a report that means
/// what the last one meant costs nothing beyond the D-Bus round trip.
struct Reports {
    changes: Mutex<Changes>,
    sink: Box<dyn Fn(PointerIntent) + Send + Sync>,
}

// The name is spelled out rather than taken from `pointer_watch::IFACE`
// because the attribute needs a literal. Those two and the string in the
// script have to agree, and nothing makes them: a mismatch would have the
// script calling into nothing, with the pointer silently never switching.
// `pointer_watch`'s own test pins the script against `IFACE`; this comment is
// the third corner.
#[interface(name = "dev.dusk.app.PointerWatch")]
impl Reports {
    /// One `"1:<class>"` or `"0:"` from the compositor.
    ///
    /// Takes no result, so the script's `callDBus` does not wait on us and a
    /// slow sink cannot stall KWin's event loop.
    fn report(&self, payload: &str) {
        let intent = intent_of_report(payload);

        // Scoped so the lock is not held across the sink, which may do
        // anything including write to a socket.
        let changed = match self.changes.lock() {
            Ok(mut changes) => changes.next(intent),
            Err(poisoned) => poisoned.into_inner().next(intent),
        };

        if let Some(intent) = changed {
            eprintln!(
                "dusk: the host's focused window {} the pointer",
                match intent {
                    PointerIntent::Locked => "has taken",
                    PointerIntent::Free => "has released",
                }
            );
            (self.sink)(intent);
        }
    }
}

/// A loaded watcher. Dropping it unloads the script.
pub struct Watcher {
    connection: Connection,
    script: PathBuf,
}

impl Watcher {
    /// Where the script is written for KWin to read.
    ///
    /// `loadScript` takes a path, so the embedded script has to land
    /// somewhere. The runtime directory rather than a temp file: it is
    /// per-user, it is cleaned up at logout, and a stable name means a
    /// restart overwrites rather than accumulating.
    fn script_path() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join("dusk-pointer-watch.js")
    }

    /// Own the name, write the script, and ask KWin to run it.
    pub async fn start<F>(sink: F) -> Result<Self, WatchError>
    where
        F: Fn(PointerIntent) + Send + Sync + 'static,
    {
        let script = Self::script_path();
        std::fs::write(&script, SCRIPT).map_err(|e| {
            WatchError::Unsupported(format!("the watcher could not be written to disk: {e}"))
        })?;

        let connection = zbus::connection::Builder::session()
            .map_err(|e| WatchError::Unsupported(format!("no session bus: {e}")))?
            .name(SERVICE)
            .map_err(|e| WatchError::Refused(format!("{SERVICE} is already taken: {e}")))?
            .serve_at(
                PATH,
                Reports {
                    changes: Mutex::new(Changes::new()),
                    sink: Box::new(sink),
                },
            )
            .map_err(|e| WatchError::Refused(format!("could not serve {PATH}: {e}")))?
            .build()
            .await
            .map_err(|e| WatchError::Unsupported(format!("no session bus: {e}")))?;

        let kwin = Proxy::new(&connection, KWIN_SERVICE, KWIN_PATH, KWIN_IFACE)
            .await
            .map_err(|e| {
                WatchError::Unsupported(format!(
                    "this desktop has no KWin to watch windows with: {e}"
                ))
            })?;

        let path = script.to_string_lossy().to_string();

        // Ignored on purpose: it fails when nothing was loaded, which is the
        // ordinary case. It exists for the one where Dusk died without
        // cleaning up and KWin still has the old copy.
        let _: Result<bool, _> = kwin.call("unloadScript", &(path.as_str(),)).await;

        let _: i32 = kwin
            .call("loadScript", &(path.as_str(),))
            .await
            .map_err(|e| WatchError::Refused(format!("loadScript: {e}")))?;

        // Loading registers; starting runs. Both are needed, which is not
        // obvious from the names and cost a round trip to find out.
        kwin.call::<_, _, ()>("start", &())
            .await
            .map_err(|e| WatchError::Refused(format!("start: {e}")))?;

        eprintln!("dusk: watching the host's focused window through KWin");

        Ok(Self { connection, script })
    }

    /// Start watching, and keep watching for as long as the process runs.
    ///
    /// Failure is not an error worth surfacing: a host whose desktop cannot
    /// be asked simply streams the way it did before, and says so once.
    pub fn spawn(state: Arc<AppState>) {
        tauri::async_runtime::spawn(async move {
            match Watcher::start(move |intent| state.set_pointer(intent)).await {
                Ok(watcher) => {
                    // Held here for the life of the process. Dropping it
                    // releases the name, after which the script's reports go
                    // nowhere — the `unloadScript` before each load is what
                    // makes a leftover from an unclean exit harmless.
                    let _watcher = watcher;
                    std::future::pending::<()>().await;
                }
                Err(err) => {
                    eprintln!("dusk: not following the host's focused window: {err}");
                }
            }
        });
    }

    /// Unload the script and forget it. Idempotent.
    pub async fn stop(self) {
        let path = self.script.to_string_lossy().to_string();
        if let Ok(kwin) = Proxy::new(&self.connection, KWIN_SERVICE, KWIN_PATH, KWIN_IFACE).await {
            let _: Result<bool, _> = kwin.call("unloadScript", &(path.as_str(),)).await;
        }
        let _ = std::fs::remove_file(&self.script);
        eprintln!("dusk: stopped watching the host's focused window");
    }
}

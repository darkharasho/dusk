//! Updating with nobody at the machine: asked for by another of the
//! person's machines, or because this one is idle and the setting is on.
//!
//! The window's own update flow lives in the UI and waits for someone to
//! press Restart. This one runs in the backend instead, because the machine
//! it is for usually has its window hidden in the tray, where a webview's
//! timers are the first thing an OS slows down. It uses the same updater,
//! the same manifest and the same signing key: nothing is ever copied
//! between machines, and a release that does not check out is not
//! installed.
//!
//! Only formats that install without an administrator update this way. A
//! `.deb` installs through the package manager, and an `.msi` installs for
//! every user of the machine, and both ask for an administrator: nobody is
//! there to say yes, and the updater has already ended Dusk by the time an
//! `.msi` asks, so Dusk would be gone until someone came by. The `.exe`
//! installer installs for the one user and asks nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::clipboard::host::{hosting, Hosting};
use crate::model::HostStatus;
use crate::startup::{self, NextLaunch};
use crate::state::AppState;

use super::UpdateChannel;

/// How often an idle machine looks for a release, when the setting is on.
const IDLE_CHECK: Duration = Duration::from_secs(60 * 60);
/// The first look after launch: late enough not to compete with discovery,
/// and long enough that a machine just restarted into an update is not
/// asked straight away whether it wants another.
const FIRST_IDLE_CHECK: Duration = Duration::from_secs(10 * 60);
/// The most the check and then the download may take. Without it a
/// connection that stalls holds [`UNDERWAY`] until Dusk is restarted by
/// hand, and every later update is refused as already going in.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Set while an update is on its way in, so two reasons to update at once
/// do not fetch it twice.
static UNDERWAY: AtomicBool = AtomicBool::new(false);

/// Holds [`UNDERWAY`] and lets it go however the update ends short of the
/// restart.
struct Underway;

impl Underway {
    fn claim() -> Option<Self> {
        (!UNDERWAY.swap(true, Ordering::SeqCst)).then_some(Underway)
    }
}

impl Drop for Underway {
    fn drop(&mut self) {
        UNDERWAY.store(false, Ordering::SeqCst);
    }
}

/// Whether this copy of Dusk can update with nobody at the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unattended {
    Possible,
    /// A `.deb`, which the package manager installs.
    PackageManager,
    /// An `.msi`, which installs for every user.
    WindowsInstaller,
    /// A debug build, or an install Dusk cannot update at all.
    Never,
}

/// How this copy could update with nobody at it.
pub fn unattended() -> Unattended {
    match super::update_channel() {
        UpdateChannel::Plugin if is_msi() => Unattended::WindowsInstaller,
        UpdateChannel::Plugin => Unattended::Possible,
        UpdateChannel::Deb => Unattended::PackageManager,
        UpdateChannel::None => Unattended::Never,
    }
}

fn is_msi() -> bool {
    matches!(
        tauri::utils::platform::bundle_type(),
        Some(tauri::utils::config::BundleType::Msi)
    )
}

/// Why this machine cannot update with nobody at it, in words for the
/// person at the machine that asked. `None` when it can.
pub fn cannot_update(how: Unattended, name: &str) -> Option<String> {
    match how {
        Unattended::Possible => None,
        Unattended::PackageManager => Some(format!(
            "{name} installs updates through its package manager, which needs \
             someone at {name} to enter the administrator password."
        )),
        Unattended::WindowsInstaller => Some(format!(
            "Dusk on {name} was installed for every user, so its updates need \
             someone at {name} to approve them."
        )),
        Unattended::Never => Some(format!("This copy of Dusk on {name} can't update itself.")),
    }
}

/// Why now is not a good time to restart this machine, in words for the
/// person at the machine that asked. `None` when nobody would lose a stream
/// but, when `own_stream` is set, the asker, who said it is streaming from
/// here and so is the one stream a restart would end.
pub async fn busy(state: &AppState, own_stream: bool) -> Option<String> {
    let name = crate::peer::host_name();
    if state.has_streams().await {
        return Some(format!(
            "{name} is streaming from another machine. Try again when that ends."
        ));
    }
    if !own_stream && !nobody_streaming_from_here(state).await {
        return Some(format!(
            "Someone is streaming from {name}. Try again when that ends."
        ));
    }
    None
}

/// Whether nobody is streaming from this machine. A check that could not
/// tell counts as somebody, unless hosting is not running at all.
async fn nobody_streaming_from_here(state: &AppState) -> bool {
    match hosting(state).await {
        Hosting::No => true,
        Hosting::Yes => false,
        Hosting::Unknown => matches!(
            state.host.probe().await,
            Ok(HostStatus::NotInstalled | HostStatus::Installed { running: false, .. })
        ),
    }
}

/// What looking for an update found.
pub enum Found {
    /// This copy is the newest release.
    UpToDate,
    /// A newer release is being fetched, after which Dusk installs it and
    /// restarts, if nobody started streaming in the meantime.
    Installing { version: String },
}

/// Look for a release and, if there is a newer one, fetch it, install it
/// and restart into it. Returns once it knows which, with the rest carrying
/// on in the background. `own_stream` is [`busy`]'s.
pub async fn update_now(
    app: &AppHandle,
    state: &Arc<AppState>,
    own_stream: bool,
) -> Result<Found, String> {
    let underway = Underway::claim().ok_or("An update is already being installed.")?;
    let updater = app
        .updater_builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(Found::UpToDate);
    };
    let version = update.version.clone();
    eprintln!("dusk: fetching {version} with nobody at the machine");
    let app = app.clone();
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = install_and_restart(&app, &state, update, own_stream).await {
            eprintln!("dusk: did not install the update: {e}");
        }
        drop(underway);
    });
    Ok(Found::Installing { version })
}

async fn install_and_restart(
    app: &AppHandle,
    state: &AppState,
    update: tauri_plugin_updater::Update,
    own_stream: bool,
) -> Result<(), String> {
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    // The fetch can take minutes, and a stream may have started in them.
    if let Some(why) = busy(state, own_stream).await {
        return Err(why);
    }

    let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    // The new Dusk comes back the way this one is now. Written before the
    // install, because on Windows the installer ends this process.
    let visible = app
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let next = if visible {
        NextLaunch::Show
    } else {
        NextLaunch::Hide
    };
    startup::mark_next_launch(&config_dir, next)?;
    if let Err(e) = update.install(bytes) {
        // A marker left behind would decide the next launch, which is
        // not this restart.
        let _ = startup::take_launch_marker(&config_dir);
        return Err(e.to_string());
    }
    super::ensure_self_executable();
    super::keep_descriptors_from_the_next_dusk();
    app.restart();
}

/// Update when idle, for as long as the setting is on. Checks once an hour,
/// and only ever installs while nobody is streaming either way.
pub fn spawn_idle_updates(app: AppHandle, state: Arc<AppState>) {
    if unattended() != Unattended::Possible {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_IDLE_CHECK).await;
        loop {
            if state.store.lock().await.auto_update.is_on() && busy(&state, false).await.is_none() {
                // On success this process restarts before the next hour.
                // Being offline is not news; the next hour tries again.
                if let Err(e) = update_now(&app, &state, false).await {
                    eprintln!("dusk: idle update check failed: {e}");
                }
            }
            tokio::time::sleep(IDLE_CHECK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_formats_that_need_no_administrator_update_with_nobody_there() {
        assert_eq!(cannot_update(Unattended::Possible, "Mini"), None);
        assert!(cannot_update(Unattended::PackageManager, "Mini")
            .unwrap()
            .contains("administrator password"));
        assert!(cannot_update(Unattended::WindowsInstaller, "Mini")
            .unwrap()
            .contains("approve"));
        assert!(cannot_update(Unattended::Never, "Mini").is_some());
    }

    #[test]
    fn an_update_that_ends_early_lets_the_next_one_in() {
        let first = Underway::claim().expect("nothing underway yet");
        assert!(Underway::claim().is_none(), "one at a time");
        drop(first);
        assert!(Underway::claim().is_some(), "free again once it ends");
    }
}

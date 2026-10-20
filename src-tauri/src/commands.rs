//! The frontend's entire surface area.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::install;
use crate::model::{Device, Snapshot};
use crate::registry::split_address;
use crate::startup;
use crate::state::{emit_snapshot, AppState};
use crate::store::ManualEntry;
use crate::sunshine::{self, Credentials, SunshineApi};

#[tauri::command]
pub async fn get_snapshot(state: State<'_, Arc<AppState>>) -> Result<Snapshot, String> {
    Ok(state.snapshot().await)
}

#[tauri::command]
pub async fn add_manual_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    address: String,
    name: Option<String>,
) -> Result<Device, String> {
    let (host, port) = split_address(&address)?;
    let name = name.and_then(|n| {
        let trimmed = n.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });

    let id = {
        let mut registry = state.registry.write().await;
        registry.upsert_manual(&host, port, name.clone())
    };

    {
        let mut store = state.store.lock().await;
        store.upsert(ManualEntry {
            address: host,
            port,
            name,
        });
        store.save(&state.store_path)?;
    }

    // Probe the new machine straight away so its card is not stuck on
    // "Checking" until the next tick.
    state.request_refresh();

    let device = state
        .registry
        .read()
        .await
        .devices()
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| "Machine was added but could not be read back.".to_string())?;

    let inner = state.inner().clone();
    emit_snapshot(&app, &inner).await;
    Ok(device)
}

#[tauri::command]
pub async fn remove_manual_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let addresses = {
        let registry = state.registry.read().await;
        registry
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| (d.addresses.clone(), d.http_port))
    };

    state.registry.write().await.remove_manual(&id)?;

    if let Some((addresses, port)) = addresses {
        let mut store = state.store.lock().await;
        for address in addresses {
            // The stored port is None when the user accepted the default, so
            // clear both spellings of the same entry.
            store.remove(&address, Some(port));
            store.remove(&address, None);
        }
        store.save(&state.store_path)?;
    }

    let inner = state.inner().clone();
    emit_snapshot(&app, &inner).await;
    Ok(())
}

#[tauri::command]
pub async fn refresh_now(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.request_refresh();
    Ok(())
}

/// A stream that ended by itself, badly.
///
/// Separate from a command's `Err` because it arrives long after the launch
/// returned — the person has already let go of the button.
pub const SESSION_FAILED_EVENT: &str = "dusk://session-failed";

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFailed {
    pub device_id: String,
    pub message: String,
}

/// How a running stream is doing, as the client measures it, about once a
/// second. Only a client that speaks the session protocol sends these.
pub const SESSION_STATS_EVENT: &str = "dusk://session-stats";

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub device_id: String,
    pub stats: crate::session::Stats,
}

/// The address to reach a device on, and its human name for messages.
async fn target_of(state: &Arc<AppState>, id: &str) -> Result<(String, String), String> {
    let registry = state.registry.read().await;
    let device = registry
        .devices()
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| "That machine is no longer in the list.".to_string())?;

    let address = device
        .primary_address
        .or_else(|| device.addresses.first().cloned())
        .ok_or_else(|| format!("Dusk has no address for {}.", device.name))?;

    Ok((address, device.name))
}

fn moonlight(state: &Arc<AppState>) -> Result<&crate::moonlight::Moonlight, String> {
    state.moonlight.as_ref().ok_or_else(|| {
        "Dusk's streaming client is missing. Reinstalling Dusk should fix this.".to_string()
    })
}

/// How pairing is going, for the drawer that started it.
pub const PAIR_PROGRESS_EVENT: &str = "dusk://pair-progress";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairProgress {
    pub device_id: String,
    /// `waitingForApproval` once the other machine's Dusk is asking someone;
    /// `manual` when no Dusk answered and the PIN has to be typed there.
    pub stage: &'static str,
}

/// Pair with a host.
///
/// Moonlight starts pairing with `pin` and waits. Meanwhile the Dusk on the
/// host, if there is one, is asked to enter the PIN itself once someone
/// there allows it — see `peer.rs`. If no Dusk answers, the person types
/// the PIN on the host, as before.
#[tauri::command]
pub async fn pair_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    pin: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    let pairing = moonlight(&inner)?.pair(&address, &pin);
    tokio::pin!(pairing);

    let progress = |stage| {
        let _ = app.emit(
            PAIR_PROGRESS_EVENT,
            PairProgress {
                device_id: id.clone(),
                stage,
            },
        );
    };
    let proof = inner.account.proof(&pin).await;
    let asking = crate::peer::ask(crate::peer::host_part(&address), &pin, proof, || {
        progress("waitingForApproval")
    });
    tokio::pin!(asking);

    let mut asked = false;
    loop {
        tokio::select! {
            done = &mut pairing => {
                done.map_err(|e| e.to_string())?;
                break;
            }
            outcome = &mut asking, if !asked => {
                asked = true;
                match outcome {
                    crate::peer::Outcome::NoPeer => progress("manual"),
                    // The PIN is in Sunshine; Moonlight finishes on its own.
                    crate::peer::Outcome::Accepted => {}
                    // Returning drops the pairing future, which stops the
                    // client rather than leaving it waiting on a PIN that
                    // is never coming.
                    crate::peer::Outcome::Declined(reason) => return Err(reason),
                }
            }
        }
    }

    // Pairing may have just minted an identity where there was none, so the
    // TLS client has to be rebuilt before the next probe can see the change.
    inner.reload_identity();
    inner.request_refresh();
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// The person at this machine allowed, or did not, another Dusk's request
/// to pair.
#[tauri::command]
pub async fn answer_pair_request(
    state: State<'_, Arc<AppState>>,
    id: u64,
    allow: bool,
) -> Result<(), String> {
    if state.peer.answer(id, allow).await {
        Ok(())
    } else {
        Err("That request has already gone away.".into())
    }
}

/// The GitHub sign-in, and the machines on its account.
#[tauri::command]
pub async fn get_account(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::account::AccountView, String> {
    Ok(state.account.view().await)
}

/// Start a GitHub sign-in. Returns the code to type at GitHub; how it ends
/// arrives later as an account event.
#[tauri::command]
pub async fn sign_in_with_github(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::account::SignInCode, String> {
    crate::account::begin(app, state.inner().clone()).await
}

#[tauri::command]
pub async fn cancel_sign_in(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    crate::account::cancel(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn sign_out_of_github(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    crate::account::sign_out(&state).await
}

/// Open GitHub's code entry page in the browser. Takes no URL from the page
/// on purpose: the one place this opens is fixed here.
#[tauri::command]
pub async fn open_sign_in_page() -> Result<(), String> {
    let url = crate::account::github::VERIFICATION_URL;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    // Not waited on with its output piped: an opener may hand off to a
    // browser that inherits the pipes and outlives any timeout.
    let mut child = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open the browser: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Start streaming an app, and watch the session for the card's sake.
#[tauri::command]
pub async fn launch_app(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    app_id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    let app_name = {
        let registry = inner.registry.read().await;
        registry
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .and_then(|d| {
                d.apps
                    .iter()
                    .find(|a| a.id == app_id)
                    .map(|a| a.name.clone())
            })
            .ok_or_else(|| "That app is no longer on this machine.".to_string())?
    };

    // The control channel is bound before the client is started, so the
    // socket is there by the time it is told where to find it. A client that
    // does not speak the protocol never connects, which costs nothing.
    let (session, effects) = crate::session::start()
        .await
        .map_err(|e| format!("could not open a control channel: {e}"))?;

    // moonlight-qt takes the app's title, not its id.
    let child = moonlight(&inner)?
        .stream(&address, &app_name, &Default::default(), &session.env())
        .map_err(|e| e.to_string())?;

    // The session ending is a state change the grid has to notice, and
    // nothing else will tell us — so wait on the child and refresh when it
    // exits rather than leaving the card mid-session until the next poll.
    // The card follows *our* session, not Sunshine's. Marked before the
    // watcher so the grid does not have a window where the stream is up
    // and the card still says Ready.
    inner.session_started(&id, session.control()).await;

    // What the in-stream overlay asks for. The overlay raises an intent and
    // Dusk decides what it means, which is what keeps the policy on this
    // side of the process boundary.
    //
    // The session is moved in here so the socket lives exactly as long as
    // the stream: when this task ends, the Session drops and the socket goes
    // with it.
    let overlay_app = app.clone();
    let stats_device = id.clone();
    // Whether Dusk stepped aside for this stream, so the watcher below knows
    // to bring it back. Shared because the two halves are separate tasks.
    let stepped_aside = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let aside = stepped_aside.clone();
    tauri::async_runtime::spawn(async move {
        let session = session;
        let mut effects = effects;
        while let Some(effect) = effects.recv().await {
            match effect {
                // Logged because nothing else shows the channel is in use:
                // the overlay's Disconnect ends the stream either way.
                //
                // It is also the moment the stream is really up, so Dusk
                // steps out of the way: the stream window becomes the one
                // thing on screen and in the taskbar, as though Dusk opened
                // it, which in effect it did. Not at launch, because a stream
                // that fails to connect would leave Dusk hidden behind
                // nothing; and not for a client without the channel, which
                // never sends this, because then nothing would say when the
                // stream is up.
                crate::session::Effect::Authenticated => {
                    eprintln!("dusk: the client connected to the session channel");
                    aside.store(true, std::sync::atomic::Ordering::Relaxed);
                    crate::tray::hide_window(&overlay_app);
                }
                crate::session::Effect::Overlay(action) => match action {
                    // Asked rather than killed: a client that is told to go
                    // can tear its stream down cleanly, and the watcher
                    // below still catches it however it actually ends.
                    crate::session::OverlayAction::Disconnect => {
                        eprintln!("dusk: the overlay asked to disconnect");
                        session.send(crate::session::ToClient::Disconnect);
                    }
                    crate::session::OverlayAction::Settings => {
                        eprintln!("dusk: the overlay asked for Dusk");
                        crate::tray::show_window(&overlay_app);
                    }
                    // Nothing to answer with yet — changing quality in
                    // flight needs the client to implement SetQuality first.
                    crate::session::OverlayAction::Quality => {}
                },
                // Passed straight through: the drawer shows them while the
                // stream is up. A dropped event is a stale second, not an
                // error worth saying anything about.
                crate::session::Effect::Stats(stats) => {
                    let _ = overlay_app.emit(
                        SESSION_STATS_EVENT,
                        SessionStats {
                            device_id: stats_device.clone(),
                            stats,
                        },
                    );
                }
                crate::session::Effect::Ended { reason } => match reason {
                    Some(reason) => eprintln!("dusk: the client ended the session: {reason}"),
                    None => eprintln!("dusk: the client ended the session"),
                },
                _ => {}
            }
        }
    });

    let handle = app.clone();
    let watched = inner.clone();
    let session_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let mut child = child;
        let ended = crate::moonlight::Moonlight::wait_for_session(&mut child).await;

        // Back where the person left it, however the stream ended — and on a
        // failure even if Dusk never stepped aside, so the reason is seen.
        if ended.is_err() || stepped_aside.load(std::sync::atomic::Ordering::Relaxed) {
            crate::tray::show_window(&handle);
        }

        if let Err(err) = ended {
            // A stream that fails after launch used to be invisible: the
            // card simply went back to Ready. Say what happened instead.
            eprintln!("dusk: the Moonlight session ended badly: {err}");
            let failed = SessionFailed {
                device_id: session_id.clone(),
                message: err.to_string(),
            };
            if let Err(err) = handle.emit(SESSION_FAILED_EVENT, failed) {
                eprintln!("dusk: could not report the failed session: {err}");
            }
        }
        // Cleared however the client ended, including a crash: leaving it
        // set would strand the card mid-session forever.
        watched.session_ended(&session_id).await;
        watched.request_refresh();
        emit_snapshot(&handle, &watched).await;
    });

    inner.request_refresh();
    Ok(())
}

// ------------------------------------------------------------- host side

/// Turn hosting on for this machine.
#[tauri::command]
pub async fn start_hosting(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    inner.host.start().await.map_err(|e| e.to_string())?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Turn hosting off for this machine.
#[tauri::command]
pub async fn stop_hosting(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    inner.host.stop().await.map_err(|e| e.to_string())?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Give Sunshine a sign-in Dusk made up, and keep it.
///
/// This replaces asking the person for the password they set in Sunshine's
/// web page, which was the last place Dusk introduced them to Sunshine. See
/// `sunshine::provision` for what it costs: whatever password Sunshine had
/// stops working.
#[tauri::command]
pub async fn provision_host(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    provision(&inner).await?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

async fn provision(inner: &Arc<AppState>) -> Result<(), String> {
    let credentials = sunshine::provision::generate()?;
    inner
        .host
        .set_credentials(&credentials)
        .await
        .map_err(|e| e.to_string())?;

    // Kept before it is checked, not after. From here this is Sunshine's
    // password whether or not the check below passes, and losing it would
    // leave a host nobody can configure until this runs again.
    inner.credentials.save(credentials.clone()).await?;

    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT).map_err(|e| e.to_string())?;
    wait_until_accepted(&api, &credentials).await?;

    // This is the moment Dusk takes over presenting this host, so it is also
    // the moment Sunshine's own tray icon should go. Doing it here and not at
    // every launch matters: applying it needs a restart, and restarting
    // Sunshine on a schedule Dusk chooses would cut off whatever is
    // streaming. Failing is not worth failing over — the sign-in works
    // either way and a second icon is a blemish, not a breakage.
    if let Err(err) = hide_sunshine_tray(&api, &credentials).await {
        eprintln!("dusk: could not hide Sunshine's tray icon: {err}");
    }
    Ok(())
}

/// Sunshine was just restarted to read the new credential, and takes a few
/// seconds to listen again. Until it does, "unreachable" means "not yet".
async fn wait_until_accepted(api: &SunshineApi, credentials: &Credentials) -> Result<(), String> {
    const ATTEMPTS: u32 = 20;
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match api.verify(credentials).await {
            Ok(_) => return Ok(()),
            // Answering and refusing is a real failure, not a slow start.
            Err(sunshine::ApiError::Unauthorized) => {
                return Err("Hosting restarted but did not accept the sign-in Dusk set.".into())
            }
            Err(err) => last = err.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(format!(
        "Hosting did not come back after restarting: {last}"
    ))
}

/// Turn Sunshine's tray icon off and restart it if that changed anything.
async fn hide_sunshine_tray(api: &SunshineApi, credentials: &Credentials) -> Result<(), String> {
    let changed = api
        .set_tray(credentials, false)
        .await
        .map_err(|e| e.to_string())?;
    if changed {
        api.restart(credentials).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

// Putting the icon back needs no command of its own: `system_tray` is an
// ordinary Sunshine setting, and the settings screen already renders, saves
// and restarts for every key the host reports. A second path to the same
// write would only be one more thing to keep in step.

/// Accept a pairing PIN on this machine.
///
/// This is the half of pairing that is Sunshine's web page today, and doing
/// it here is the single biggest reason Dusk stops feeling like two
/// programs.
#[tauri::command]
pub async fn accept_pin(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    pin: String,
    device_name: Option<String>,
) -> Result<(), String> {
    let inner = state.inner().clone();

    let pin = pin.trim().to_string();
    if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err("A pairing PIN is four digits.".into());
    }

    let credentials = inner
        .credentials
        .load()
        .await
        .ok_or_else(|| sunshine::ApiError::NoCredentials.to_string())?;

    let name = device_name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Dusk".to_string());

    SunshineApi::new(sunshine::api::DEFAULT_PORT)
        .map_err(|e| e.to_string())?
        .submit_pin(&credentials, &pin, &name)
        .await
        .map_err(|e| e.to_string())?;

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// The first-run checklist for this machine.
#[tauri::command]
pub async fn get_setup(state: State<'_, Arc<AppState>>) -> Result<install::Setup, String> {
    let inner = state.inner().clone();
    let host = inner.host.state().await;
    Ok(install::steps::build(
        host.platform,
        &host.status,
        host.capabilities.support_tier,
        inner.credentials.load().await.is_some(),
        install::apply::linux_input_ready(),
        install::display_driver::installed(),
    ))
}

/// Which Sunshine build this machine would get, without downloading it.
///
/// Separate from installing on purpose: it is worth being able to see the
/// version, the size and whether a checksum exists before committing to a
/// download that will be run with elevated rights.
#[tauri::command]
pub async fn preview_sunshine_download(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<DownloadPreview, String> {
    if let Some(helper) = install::embedded::find() {
        return Ok(DownloadPreview {
            version: install::embedded::version(&helper).unwrap_or_default(),
            asset: install::embedded::HELPER.into(),
            size: 0,
            verifiable: true,
            bundled: true,
            embedded: true,
        });
    }

    let resources = app.path().resource_dir().ok();
    if let Some(bundled) = install::bundled::find(resources.as_deref()) {
        return Ok(DownloadPreview {
            version: bundled.version,
            asset: bundled
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size: bundled.size,
            verifiable: true,
            bundled: true,
            embedded: false,
        });
    }

    let inner = state.inner().clone();
    let choice = latest_choice(&inner).await?;
    Ok(DownloadPreview {
        version: choice.0,
        asset: choice.1.asset.name.clone(),
        size: choice.1.asset.size,
        verifiable: choice.1.asset.sha256.is_some(),
        bundled: false,
        embedded: false,
    })
}

/// The newest Sunshine release and the asset in it for this machine.
async fn latest_choice(
    inner: &Arc<AppState>,
) -> Result<(String, install::release::Choice), String> {
    let release = install::release::fetch_latest(&inner.http).await?;
    let choice = install::release::select(
        &release.assets,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .ok_or_else(|| {
        format!(
            "Hosting is not available for {} on {}.",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    })?;
    Ok((release.version, choice))
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPreview {
    pub version: String,
    pub asset: String,
    pub size: u64,
    /// False when the release carries no checksum, in which case Dusk
    /// refuses to install rather than running an unverifiable binary.
    pub verifiable: bool,
    /// True when the installer ships inside Dusk and nothing is downloaded.
    pub bundled: bool,
    /// True on macOS, where hosting itself is inside Dusk and installing it
    /// copies nothing — so there is no size to show and no file to hash.
    pub embedded: bool,
}

/// Progress events while a release downloads.
pub const DOWNLOAD_EVENT: &str = "dusk://download";

/// Install Sunshine: on macOS the helper inside Dusk, elsewhere the
/// installer inside Dusk, otherwise the newest release, downloaded and
/// checked.
///
/// Both halves are real state changes on the machine, so this only ever runs
/// from an explicit press — never on a timer, and never as part of a probe.
#[tauri::command]
pub async fn install_sunshine(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let inner = state.inner().clone();

    let resources = app.path().resource_dir().ok();
    if let Some(helper) = install::embedded::find() {
        // Signed and sealed inside Dusk's own bundle, so macOS has already
        // checked it; there is no separate file to hash.
        install::apply::install_embedded(&helper)
            .await
            .map_err(|e| e.to_string())?;
    } else if let Some(bundled) = install::bundled::find(resources.as_deref()) {
        install::bundled::verify(&bundled).await?;
        install::apply::install(&bundled.path, bundled.kind)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        download_and_install(&app, &inner).await?;
    }

    // A Sunshine Dusk just installed has no sign-in yet, and nobody should
    // have to make one up. Skipped when Dusk already holds one, so
    // reinstalling over a working host does not replace its password.
    let provisioned = if inner.credentials.load().await.is_none() {
        provision(&inner).await.map_err(|e| {
            format!("Hosting is installed, but Dusk could not finish setting it up: {e}")
        })
    } else {
        Ok(())
    };

    emit_snapshot(&app, &inner).await;
    provisioned
}

/// The fallback for a build with no installer inside it: fetch the newest
/// release, check it, and install it.
async fn download_and_install(app: &AppHandle, inner: &Arc<AppState>) -> Result<(), String> {
    let (_, choice) = latest_choice(inner).await?;

    // Downloads land in the app's own cache directory rather than a shared
    // temp path: a verified installer briefly sitting somewhere world
    // writable is a place to swap it before it runs.
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("downloads");

    let emitter = app.clone();
    let path = install::download::fetch(&inner.http, &choice.asset, &dir, move |progress| {
        let _ = emitter.emit(DOWNLOAD_EVENT, progress);
    })
    .await
    .map_err(|e| e.to_string())?;

    let outcome = install::apply::install(&path, choice.kind)
        .await
        .map_err(|e| e.to_string());

    // The installer is a verified copy of a public release, not a secret,
    // but there is no reason to leave 40MB behind either.
    let _ = tokio::fs::remove_file(&path).await;
    outcome
}

/// Install the virtual display driver. Windows only; asks for administrator
/// rights once. Only ever run from its own button, never as part of setting
/// up everything at once.
#[tauri::command]
pub async fn install_display_driver(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("downloads");
    install::display_driver::install(&state.http, &dir).await
}

/// Let Sunshine create input devices. Linux only; asks through polkit.
#[tauri::command]
pub async fn allow_input() -> Result<(), String> {
    install::apply::linux_allow_input()
        .await
        .map_err(|e| e.to_string())
}

/// Add firewall rules for Sunshine. Windows only; elevates when it runs.
#[tauri::command]
pub async fn open_firewall() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let program = std::env::var_os("ProgramFiles")
            .map(|base| std::path::PathBuf::from(base).join("Sunshine\\sunshine.exe"))
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                "Dusk could not find sunshine.exe, so it cannot add a rule for it.".to_string()
            })?;
        install::apply::open_firewall(&program)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Dusk only manages firewall rules on Windows.".into())
    }
}

/// Open the system screen where a permission Dusk cannot grant is granted.
///
/// macOS TCC settings cannot be set by any installer, so the most Dusk can
/// do is put the right pane in front of someone.
#[tauri::command]
pub async fn open_privacy_settings(pane: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let anchor = match pane.as_str() {
            "screenRecording" => "Privacy_ScreenCapture",
            "accessibility" => "Privacy_Accessibility",
            _ => return Err("Dusk does not know that settings pane.".into()),
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        let out = crate::host::service::run("open", &[&url])
            .await
            .map_err(|e| e.to_string())?;
        if !out.ok() {
            return Err(out.message());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("This only applies to macOS.".into())
    }
}

async fn host_api(state: &Arc<AppState>) -> Result<(SunshineApi, Credentials), String> {
    let credentials = state
        .credentials
        .load()
        .await
        .ok_or_else(|| sunshine::ApiError::NoCredentials.to_string())?;
    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT).map_err(|e| e.to_string())?;
    Ok((api, credentials))
}

/// Read this machine's Sunshine configuration.
#[tauri::command]
pub async fn get_host_config(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let inner = state.inner().clone();
    let (api, credentials) = host_api(&inner).await?;
    api.get_config(&credentials)
        .await
        .map_err(|e| e.to_string())
}

/// Write changed settings, and optionally restart so they take effect.
#[tauri::command]
pub async fn save_host_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    changes: serde_json::Map<String, serde_json::Value>,
    restart: bool,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (api, credentials) = host_api(&inner).await?;

    api.save_config(&credentials, changes)
        .await
        .map_err(|e| e.to_string())?;

    if restart {
        api.restart(&credentials).await.map_err(|e| e.to_string())?;
    }

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Leave the stream running from this machine, without ending what it is
/// running. Only a client that speaks the session protocol hears this; the
/// window only offers it once that client has said something.
#[tauri::command]
pub async fn leave_stream(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let control = state
        .session_control(&id)
        .await
        .ok_or_else(|| "Nothing is streaming from that machine.".to_string())?;
    control.send(crate::session::ToClient::Disconnect);
    Ok(())
}

/// Ask the host to end whatever session is running on it.
#[tauri::command]
pub async fn quit_session(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    moonlight(&inner)?
        .quit(&address)
        .await
        .map_err(|e| e.to_string())?;

    inner.request_refresh();
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Whether Dusk opens at sign-in, and how it opens and closes.
#[tauri::command]
pub async fn get_startup(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<startup::StartupView, String> {
    let prefs = state.store.lock().await.startup;
    Ok(startup::view(&app, prefs))
}

#[tauri::command]
pub async fn set_open_at_login(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    on: bool,
) -> Result<startup::StartupView, String> {
    startup::set_open_at_login(&app, on)?;
    get_startup(app, state).await
}

#[tauri::command]
pub async fn set_startup_prefs(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    prefs: startup::Prefs,
) -> Result<startup::StartupView, String> {
    {
        let mut store = state.store.lock().await;
        store.startup = prefs;
        store.save(&state.store_path)?;
    }
    app.state::<startup::Live>().set(&prefs);
    get_startup(app, state).await
}

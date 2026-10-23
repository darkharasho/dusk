//! The frontend's entire surface area.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::install;
use crate::model::{Device, HostStatus, Snapshot};
use crate::registry::split_address;
use crate::startup;
use crate::state::{emit_snapshot, AppState};
use crate::store::{AutoUpdate, ClipboardSharing, Codec, ManualEntry, Resolution, BITRATE_RANGE};
use crate::sunshine::{self, Credentials, Known, SunshineApi};

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
pub async fn refresh_now(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // The account's list too, not only the network: Refresh is the one
    // thing a person reaches for when a machine they just signed in is
    // missing from the grid. Not awaited: GitHub may take its whole
    // timeout to not answer, and the local probe should not wait on that.
    // The read asks for a tick of its own when it lands.
    let inner = state.inner().clone();
    tokio::spawn(async move {
        inner.account.refresh_now(&inner.http).await;
        inner.request_refresh();
    });
    state.request_refresh();
    crate::discovery::sweep(&app, state.inner());
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
    /// `manual` when no Dusk answered and the PIN has to be typed there;
    /// `waitingForHost` when the other machine paired this one but is
    /// streaming, and finishes once that stream ends.
    /// `clientOpen` when a stream window here has to close before this
    /// machine can finish its half.
    pub stage: &'static str,
}

/// Pair with a host.
///
/// A host on your own account is paired first by trading certificates, with
/// no PIN at all — see `cert_pairing::client`. Otherwise Moonlight starts
/// pairing with `pin` and waits. Meanwhile the Dusk on the host, if there is
/// one, is asked to enter the PIN itself once someone there allows it — see
/// `peer.rs`. If no Dusk answers, the person types the PIN on the host, as
/// before.
#[tauri::command]
pub async fn pair_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    pin: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    let progress = |stage| {
        let _ = app.emit(
            PAIR_PROGRESS_EVENT,
            PairProgress {
                device_id: id.clone(),
                stage,
            },
        );
    };
    let by_cert = crate::cert_pairing::client::pair(
        &inner,
        &address,
        &id,
        || progress("clientOpen"),
        || progress("waitingForHost"),
    );
    if by_cert.await? {
        inner.reload_identity();
        inner.request_refresh();
        emit_snapshot(&app, &inner).await;
        return Ok(());
    }

    let pairing = moonlight(&inner)?.pair(&address, &pin);
    tokio::pin!(pairing);
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

/// What a stream asks for when the size it should match cannot be found:
/// a host without Dusk, or one too old to say. The commonest screen there
/// is, and a size every host can encode.
const FALLBACK_SIZE: (u32, u32) = (1920, 1080);

/// The size to stream at, from the Streaming setting.
async fn stream_size(app: &AppHandle, state: &Arc<AppState>, address: &str) -> (u32, u32) {
    let resolution = state.store.lock().await.resolution;
    // Asked whatever the setting, since the question also wakes a Mac's
    // screen; only Match host waits for the answer.
    if resolution != Resolution::MatchHost {
        let host = crate::peer::host_part(address).to_string();
        let screen = this_display(app);
        tauri::async_runtime::spawn(async move {
            crate::peer::display_size(&host, screen).await;
        });
    }
    let size = match resolution {
        Resolution::MatchHost => {
            crate::peer::display_size(crate::peer::host_part(address), this_display(app)).await
        }
        Resolution::MatchThisDisplay => this_display(app),
        Resolution::Fixed { width, height } => Some((width, height)),
    };
    // Encoders want even dimensions; a screen with an odd one is rare, but
    // one pixel less is better than a stream that will not start.
    let (width, height) = size.unwrap_or(FALLBACK_SIZE);
    (width & !1, height & !1)
}

/// The screen Dusk's window is on, which is where the stream will open.
fn this_display(app: &AppHandle) -> Option<(u32, u32)> {
    let window = app.get_webview_window("main")?;
    let monitor = match window.current_monitor() {
        Ok(Some(monitor)) => monitor,
        _ => app.primary_monitor().ok()??,
    };
    let size = monitor.size();
    (size.width > 0 && size.height > 0).then_some((size.width, size.height))
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardUnavailable {
    pub device_id: String,
    pub message: String,
}

/// What a person is told when the host's desktop cannot share a clipboard.
fn unavailable_message(detail: &Option<String>) -> &'static str {
    if detail.is_some() {
        "Clipboard sharing isn't available when that machine runs GNOME on Wayland."
    } else {
        "Clipboard sharing isn't available on that machine yet."
    }
}

/// The pinned TLS setup for sharing a clipboard with the host streamed from:
/// its certificate as the streaming client stored it, and this machine's
/// client identity. `None` (after one log line) when either is missing.
async fn clipboard_tls(
    state: &Arc<AppState>,
    device_id: &str,
    address: &str,
) -> Option<Arc<rustls::ClientConfig>> {
    let (id, address) = (device_id.to_string(), address.to_string());
    let state = state.clone();
    let found = tokio::task::spawn_blocking(move || {
        let client = state.moonlight.as_ref();
        let hosts = crate::moonlight::hosts::load(client);
        let Some(cert) = pinned_cert(&hosts, &id, &address) else {
            eprintln!("dusk: not sharing the clipboard: no pinned certificate for this host");
            return None;
        };
        let Some(identity) = crate::moonlight::identity::load(client) else {
            eprintln!("dusk: not sharing the clipboard: no client identity");
            return None;
        };
        match crate::clipboard::tls::client_config(&identity, cert) {
            Ok(config) => Some(config),
            Err(_) => {
                // Fixed text: the error is about key material.
                eprintln!("dusk: not sharing the clipboard: the stored certificates are unusable");
                None
            }
        }
    })
    .await
    .ok()
    .flatten();
    found
}

/// The certificate a host is pinned by: matched on its unique id first, and
/// only failing that on its address (exact before host part alone).
fn pinned_cert<'a>(
    hosts: &'a [crate::moonlight::hosts::KnownHost],
    id: &str,
    address: &str,
) -> Option<&'a str> {
    let wanted = crate::peer::host_part(address);
    hosts
        .iter()
        .find(|h| h.uuid.as_deref() == Some(id))
        .or_else(|| {
            hosts
                .iter()
                .find(|h| h.addresses.iter().any(|a| a == address))
        })
        .or_else(|| {
            hosts.iter().find(|h| {
                h.addresses
                    .iter()
                    .any(|a| crate::peer::host_part(a) == wanted)
            })
        })
        .and_then(|h| h.server_cert.as_deref())
}

/// A clipboard connection that is up or on its way: copies go in the
/// sender, and how it ended, and when, comes back on the receiver.
type ClipboardRun = (
    tokio::sync::mpsc::UnboundedSender<String>,
    tokio::sync::oneshot::Receiver<(crate::clipboard::client::LastEnd, tokio::time::Instant)>,
);

/// Share the clipboard with the host for as long as the returned sender
/// lives and the setting stays on. Dropping it ends the connection.
fn start_clipboard(
    app: &AppHandle,
    state: Arc<AppState>,
    control: crate::session::Control,
    device_id: String,
    config: Arc<rustls::ClientConfig>,
    address: String,
    offs: u64,
) -> ClipboardRun {
    use crate::clipboard::{client, Decline};
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let ended = match client::connect(&address, config).await {
            Ok(stream) => {
                // Turned off since this began counts as off, even if it is
                // on again: the run ends, and the next one starts afresh.
                let sharing_on = move || {
                    let state = state.clone();
                    async move { clipboard_sharing(&state).await == (true, offs) }
                };
                client::run(stream, control, rx, sharing_on).await
            }
            Err(ended) => ended,
        };
        let _ = done_tx.send((client::LastEnd::of(&ended), tokio::time::Instant::now()));
        match ended {
            client::Ended::Declined(Decline::Unsupported { detail }) => {
                let _ = app.emit(
                    client::UNAVAILABLE_EVENT,
                    ClipboardUnavailable {
                        device_id,
                        message: unavailable_message(&detail).to_string(),
                    },
                );
            }
            // Logged by kind only: nothing that was copied.
            other => eprintln!("dusk: clipboard sharing ended: {other:?}"),
        }
    });
    (tx, done_rx)
}

/// Whether clipboard sharing is on here, and how many times it has been
/// turned off, read together.
async fn clipboard_sharing(state: &AppState) -> (bool, u64) {
    let store = state.store.lock().await;
    let offs = state
        .clipboard_turned_off
        .load(std::sync::atomic::Ordering::SeqCst);
    (store.clipboard_sharing.is_on(), offs)
}

/// How often a stream without a clipboard connection checks whether to
/// make one.
const CLIPBOARD_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// One stream's clipboard connection, made once the stream is up and made
/// again when it drops, by the rules in
/// [`crate::clipboard::client::should_reconnect`].
struct ClipboardLink {
    app: AppHandle,
    state: Arc<AppState>,
    control: crate::session::Control,
    device_id: String,
    address: String,
    /// Whether the stream is up: nothing is connected before.
    up: bool,
    live: Option<ClipboardRun>,
    /// The latest copy made while no connection was up.
    held: crate::clipboard::client::Held,
    last: crate::clipboard::client::LastEnd,
    ended_at: tokio::time::Instant,
}

impl ClipboardLink {
    fn new(
        app: AppHandle,
        state: Arc<AppState>,
        control: crate::session::Control,
        device_id: String,
        address: String,
    ) -> Self {
        Self {
            app,
            state,
            control,
            device_id,
            address,
            up: false,
            live: None,
            held: Default::default(),
            // Nothing tried yet: connect as soon as sharing is on.
            last: crate::clipboard::client::LastEnd::OwnSetting,
            ended_at: tokio::time::Instant::now(),
        }
    }

    fn ended(&mut self, last: crate::clipboard::client::LastEnd, at: tokio::time::Instant) {
        self.live = None;
        self.last = last;
        self.ended_at = at;
    }

    /// Notes a connection that ended since last looked, connects when the
    /// rules allow, and drops the connection when sharing was turned off
    /// here, which ends it.
    async fn tend(&mut self) {
        use crate::clipboard::client::{should_reconnect, LastEnd};
        use tokio::sync::oneshot::error::TryRecvError;
        if !self.up {
            return;
        }
        if let Some((_, done)) = &mut self.live {
            match done.try_recv() {
                Ok((last, at)) => self.ended(last, at),
                // Ended without saying how: treated as the host going.
                Err(TryRecvError::Closed) => {
                    self.ended(LastEnd::HostSide, tokio::time::Instant::now())
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let (sharing, offs) = clipboard_sharing(&self.state).await;
        if !sharing {
            // A copy from before sharing went off is never sent.
            self.held.clear();
        }
        if self.live.is_some() {
            if !sharing {
                self.ended(LastEnd::OwnSetting, tokio::time::Instant::now());
            }
            return;
        }
        if !should_reconnect(&self.last, self.ended_at.elapsed(), sharing) {
            return;
        }
        match clipboard_tls(&self.state, &self.device_id, &self.address).await {
            Some(config) => {
                let (tx, done) = start_clipboard(
                    &self.app,
                    self.state.clone(),
                    self.control.clone(),
                    self.device_id.clone(),
                    config,
                    self.address.clone(),
                    offs,
                );
                // Sent once the host admits it, if sharing is still on then.
                if let Some(text) = self.held.take(sharing, offs) {
                    let _ = tx.send(text);
                }
                self.live = Some((tx, done));
            }
            // Already logged; it will not appear during this stream.
            None => {
                self.held.clear();
                self.ended(LastEnd::GiveUp, tokio::time::Instant::now());
            }
        }
    }

    /// A copy from the stream window: sent on, over a connection made for
    /// it if the rules allow one now. Otherwise the latest copy is held for
    /// the next connection, as long as sharing stays on here.
    async fn copied(&mut self, text: String) {
        self.tend().await;
        if let Some((tx, _)) = &self.live {
            let _ = tx.send(text);
            return;
        }
        let (sharing, offs) = clipboard_sharing(&self.state).await;
        // Nothing is held for a stream that will not connect again.
        if sharing && self.up && self.last != crate::clipboard::client::LastEnd::GiveUp {
            self.held.keep(text, offs);
        }
    }
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

    let (bitrate, codec) = {
        let store = inner.store.lock().await;
        (store.bitrate_kbps, store.codec)
    };
    let options = crate::moonlight::cli::StreamOptions {
        resolution: Some(stream_size(&app, &inner, &address).await),
        bitrate,
        codec: (codec != Codec::Auto).then_some(codec),
        ..Default::default()
    };

    // moonlight-qt takes the app's title, not its id.
    let child = moonlight(&inner)?
        .stream(&address, &app_name, &options, &session.env())
        .map_err(|e| e.to_string())?;

    // The session ending is a state change the grid has to notice, and
    // nothing else will tell us — so wait on the child and refresh when it
    // exits rather than leaving the card mid-session until the next poll.
    // The card follows *our* session, not Sunshine's. Marked before the
    // watcher so the grid does not have a window where the stream is up
    // and the card still says Ready.
    inner
        .session_started(&id, session.control(), child.id())
        .await;

    // What the in-stream overlay asks for. The overlay raises an intent and
    // Dusk decides what it means, which is what keeps the policy on this
    // side of the process boundary.
    //
    // The session is moved in here so the socket lives exactly as long as
    // the stream: when this task ends, the Session drops and the socket goes
    // with it.
    let overlay_app = app.clone();
    let stats_device = id.clone();
    let mut clipboard = ClipboardLink::new(
        app.clone(),
        inner.clone(),
        session.control(),
        id.clone(),
        address.clone(),
    );
    // Whether Dusk stepped aside for this stream, so the watcher below knows
    // to bring it back. Shared because the two halves are separate tasks.
    let stepped_aside = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let aside = stepped_aside.clone();
    tauri::async_runtime::spawn(async move {
        let session = session;
        let mut effects = effects;
        // The clipboard link is held for as long as the stream is: when this
        // loop ends its sender drops, which ends the clipboard connection.
        let mut tick =
            tokio::time::interval_at(tokio::time::Instant::now() + CLIPBOARD_TICK, CLIPBOARD_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let effect = tokio::select! {
                effect = effects.recv() => match effect {
                    Some(effect) => effect,
                    None => break,
                },
                _ = tick.tick() => {
                    clipboard.tend().await;
                    continue;
                }
            };
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

                    clipboard.up = true;
                    clipboard.tend().await;
                }
                // Sent on, over a new connection if none is up and the
                // rules allow one. Sharing turned off here mid-stream: the
                // connection goes, and nothing more is passed on.
                crate::session::Effect::Clipboard(text) => clipboard.copied(text).await,
                crate::session::Effect::Overlay(action) => match action {
                    // Asked rather than killed: a client that is told to go
                    // can tear its stream down cleanly, and the watcher
                    // below still catches it however it actually ends.
                    crate::session::OverlayAction::Disconnect => {
                        eprintln!("dusk: the overlay asked to disconnect");
                        session.send(crate::session::ToClient::Disconnect);
                    }
                    // The client quits the host's app on its way out, as it
                    // does for its own quit-and-exit combo, so there is
                    // nothing to add here beyond seeing it go.
                    crate::session::OverlayAction::EndSession => {
                        eprintln!("dusk: the overlay asked to end the session on the host");
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
        let log = crate::moonlight::stream_log::StreamLog::create();
        if let Some(log) = &log {
            log.line(&format!("dusk: streaming {app_name} from {address}"));
        }
        let ended = crate::moonlight::Moonlight::wait_for_session(&mut child, log).await;

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

/// Whether Dusk definitely holds no sign-in for the host.
///
/// Only a keystore that answered counts. One that would not (locked
/// keyring, dismissed prompt, no secret service in this session) is an
/// error here rather than a "no": the callers set a fresh password and
/// restart the host on "no", which would cut a stream and replace a
/// password that was fine.
async fn no_password_stored(inner: &AppState) -> Result<bool, String> {
    inner
        .credentials
        .try_load()
        .await
        .map(|credentials| credentials.is_none())
        .map_err(|err| {
            format!("Dusk could not open the system keystore to check hosting's sign-in: {err}. Unlock it and try again.")
        })
}

/// Turn hosting on for this machine.
#[tauri::command]
pub async fn start_hosting(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    // A host Dusk holds no sign-in for gets one on the way up. Setting it
    // restarts the service, and restarting a stopped one starts it, so this
    // is the start too, and on Windows it is one elevation prompt, not two.
    if no_password_stored(&inner).await? {
        provision(&inner).await?;
    } else {
        inner.host.start().await.map_err(|e| e.to_string())?;
    }
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

/// Take over a running host Dusk holds no sign-in for, without asking.
///
/// Dusk's own sign-in is GitHub; the host's password is plumbing, and the
/// person should never meet it. So a host someone installed before Dusk, or
/// one whose password was lost with a keystore, is simply given a new one.
///
/// Runs once, after the keystore read. Not on Windows, where setting the
/// password needs an elevation prompt nobody asked for; there it happens
/// the next time hosting is turned on, or from Settings. Not for a stopped
/// host either, since setting the password restarts it and a host someone
/// turned off should stay off.
pub async fn adopt_host(app: &AppHandle, inner: &Arc<AppState>) {
    if cfg!(windows) || inner.credentials.peek().await != Known::SignedOut {
        return;
    }
    if !matches!(
        inner.host.probe().await,
        Ok(HostStatus::Installed { running: true, .. })
    ) {
        return;
    }
    match provision(inner).await {
        Ok(()) => emit_snapshot(app, inner).await,
        Err(err) => eprintln!("dusk: could not set up hosting on its own: {err}"),
    }
}

async fn provision(inner: &Arc<AppState>) -> Result<(), String> {
    sunshine::provision::run(inner.host.as_ref(), &inner.credentials).await
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
    if !inner.store.lock().await.allow_other_devices {
        return Err("Turn on Let other devices pair first.".into());
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

    // The same patient entry the peer path uses: the other machine's
    // client may not have asked the host yet when the PIN is typed here,
    // and the host refuses a PIN nobody is waiting on. No address to go
    // on, so every waiting pairing is offered it.
    crate::peer::enter_pin(&credentials, None, &pin, &name)
        .await
        .map_err(|e| e.to_string())?;

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Whether machines outside this one's GitHub account may pair with it.
#[tauri::command]
pub async fn set_allow_other_devices(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    on: bool,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().await;
        store.allow_other_devices = on;
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
    Ok(())
}

/// Whether streams from this machine show the virtual display, sized to
/// the connecting machine. Turning it on asks for administrator rights
/// once, to restart the driver with the sizes it needs.
#[tauri::command]
pub async fn set_virtual_screen(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    on: bool,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (api, credentials) = host_api(&inner).await?;
    if on {
        crate::host::virtual_screen::enable(&api, &credentials).await?;
    } else {
        crate::host::virtual_screen::disable(&api, &credentials).await?;
    }
    {
        let mut store = inner.store.lock().await;
        store.virtual_screen = on;
        store.save(&inner.store_path)?;
    }
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// How big streams from this machine ask to be.
#[tauri::command]
pub async fn set_resolution(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    resolution: Resolution,
) -> Result<(), String> {
    if !resolution.is_valid() {
        return Err("That is not a size a stream can be.".into());
    }
    {
        let mut store = state.store.lock().await;
        store.resolution = resolution;
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
    Ok(())
}

/// How much data streams from this machine may use, in Kbps, or `None` to
/// let the client pick from the stream's size and frame rate.
#[tauri::command]
pub async fn set_bitrate(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    kbps: Option<u32>,
) -> Result<(), String> {
    if kbps.is_some_and(|k| !BITRATE_RANGE.contains(&k)) {
        return Err("Bitrate has to be between 0.5 and 500 Mbps.".into());
    }
    {
        let mut store = state.store.lock().await;
        store.bitrate_kbps = kbps;
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
    Ok(())
}

/// Which video format streams from this machine ask the host for.
#[tauri::command]
pub async fn set_codec(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    codec: Codec,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().await;
        store.codec = codec;
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
    Ok(())
}

/// Whether text copied during a stream is shared with the other machine.
#[tauri::command]
pub async fn set_clipboard_sharing(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    on: bool,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().await;
        store.clipboard_sharing = if on {
            ClipboardSharing::On
        } else {
            // Under the store lock, so it is read together with the setting.
            state
                .clipboard_turned_off
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ClipboardSharing::Off
        };
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
    Ok(())
}

/// Whether Dusk updates itself while nobody is streaming either way.
#[tauri::command]
pub async fn set_auto_update(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    on: bool,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().await;
        store.auto_update = if on { AutoUpdate::On } else { AutoUpdate::Off };
        store.save(&state.store_path)?;
    }
    emit_snapshot(&app, state.inner()).await;
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
    let provisioned = if no_password_stored(&inner).await? {
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

/// Add firewall rules for Sunshine and Dusk. Windows only; elevates when it runs.
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
        let dusk = std::env::current_exe()
            .map_err(|e| format!("Dusk could not find its own program file: {e}"))?;
        install::apply::open_firewall(&program, &dusk)
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

/// Ask another of your machines to update its Dusk, and restart into it.
#[tauri::command]
pub async fn update_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<crate::peer::UpdateOutcome, String> {
    let inner = state.inner().clone();
    let machine = {
        let registry = inner.registry.read().await;
        let device = registry
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| "That machine is no longer in the list.".to_string())?;
        let machine = device
            .machine_id
            .filter(|_| device.mine)
            .ok_or_else(|| format!("{} isn't one of your machines.", device.name))?;
        machine
    };
    // From the sessions, not the device: the registry's copy of whether
    // this machine streams from it is only filled in for the snapshot.
    let streaming = inner.is_streaming_from(&id).await;
    let (address, _) = target_of(&inner, &id).await?;
    let host = crate::peer::host_part(&address).to_string();
    let outcome = crate::peer::ask_update(&inner, &host, &machine, streaming).await?;
    inner.request_refresh();
    emit_snapshot(&app, &inner).await;
    Ok(outcome)
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

#[cfg(test)]
mod clipboard_lookup_tests {
    use super::{pinned_cert, unavailable_message};
    use crate::moonlight::hosts::KnownHost;

    #[test]
    fn a_decline_with_a_detail_names_the_desktop_and_one_without_does_not() {
        assert_eq!(
            unavailable_message(&Some("anything".into())),
            "Clipboard sharing isn't available when that machine runs GNOME on Wayland."
        );
        assert_eq!(
            unavailable_message(&None),
            "Clipboard sharing isn't available on that machine yet."
        );
    }

    fn host(uuid: Option<&str>, addresses: &[&str], cert: &str) -> KnownHost {
        KnownHost {
            name: "h".into(),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            port: 47989,
            uuid: uuid.map(str::to_string),
            server_cert: Some(cert.into()),
        }
    }

    #[test]
    fn the_uuid_wins_over_an_earlier_entry_sharing_the_address() {
        let hosts = [
            host(Some("other"), &["10.0.0.5"], "stale"),
            host(Some("me"), &["10.0.0.9"], "right"),
        ];
        assert_eq!(pinned_cert(&hosts, "me", "10.0.0.5"), Some("right"));
    }

    #[test]
    fn an_exact_address_beats_a_host_part_match() {
        let hosts = [
            host(None, &["10.0.0.5:47989"], "by-host-part"),
            host(None, &["10.0.0.5"], "exact"),
        ];
        assert_eq!(pinned_cert(&hosts, "none", "10.0.0.5"), Some("exact"));
    }

    #[test]
    fn nothing_matching_gives_nothing() {
        let hosts = [host(Some("a"), &["10.0.0.1"], "x")];
        assert_eq!(pinned_cert(&hosts, "b", "10.0.0.2"), None);
    }
}

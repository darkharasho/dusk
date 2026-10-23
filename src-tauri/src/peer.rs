//! Dusk talking to Dusk, so pairing stops involving a PIN.
//!
//! Moonlight pairing needs the same four digits typed on both ends, and that
//! is the most "two programs" moment left. But when Dusk runs on both
//! machines it can carry the PIN across itself: the machine connecting makes
//! one up, starts pairing with it, and hands it to the Dusk on the host,
//! which enters it into Sunshine once someone there allows it.
//!
//! # The trust decision
//!
//! Nothing on the network may pair itself to a machine unattended. What
//! stands in for the PIN is the GitHub account: a machine signed in to the
//! same account as this one sends a signature over the PIN, made with a key
//! whose public half is on the account's machine list. When that checks
//! out, the person already said yes by signing both machines in, and
//! nothing is asked. See [`crate::account`].
//!
//! Between two machines on the same account, a certificate pairing comes
//! first: each sends the other its certificate, signed for the receiver, and
//! no PIN is involved (see [`crate::cert_pairing`]). The PIN flow below is
//! what everything else uses, and what a certificate pairing falls back to
//! when the other side is too old, declines, or does not finish.
//!
//! Anyone else is turned away, unless this machine has Let other devices
//! pair on ([`crate::store::Store::allow_other_devices`]). Then the host
//! asks instead: one Allow press, showing which machine is asking — the
//! consent the PIN used to stand for. It is off by default because it needs
//! someone at the host, and pairing your own machines never should.
//!
//! The PIN crosses the network in the clear, so someone already able to
//! intercept and rewrite traffic on the LAN at that moment could interpose
//! on the pairing. That is the same network Dusk already trusts for
//! discovery, and Dusk is scoped to the LAN or the person's own VPN; it is
//! written down here so it is a known edge rather than a surprise.
//!
//! # Protocol
//!
//! One TCP connection per request, newline-delimited JSON:
//!
//! ```text
//! connecting → host   {"type":"pair","pin":"1234","name":"Mac mini",
//!                      "proof":{"account":1,"machine":"…","signature":"…"}}
//!                                                   proof only when signed in
//! host → connecting   {"type":"waiting"}            someone is being asked
//!                     {"type":"accepted"}           the PIN is in Sunshine
//!                     {"type":"declined","reason":"..."}
//! ```
//!
//! A signed-in machine can instead pair with no PIN at all, by trading
//! certificates:
//!
//! ```text
//! connecting → host   {"type":"pair_cert","name":"Mac mini","cert":"-----BEGIN…",
//!                      "proof":{"account":1,"machine":"…","signature":"…"}}
//! host → connecting   {"type":"paired","server_cert":"-----BEGIN…","uniqueid":"…",
//!                      "ready":true,"proof":{"account":1,"machine":"…","signature":"…"}}
//!                     {"type":"declined","reason":"..."}
//! ```
//!
//! A Dusk too old to know `pair_cert` declines it like any other request it
//! does not understand.
//!
//! A connection refused means no Dusk is listening there, and the
//! connecting side falls back to showing the PIN.
//!
//! The same port answers one other question, asked before a stream so it
//! can match the host's screen:
//!
//! ```text
//! connecting → host   {"type":"display","screen":{"width":3440,"height":1440}}
//! host → connecting   {"type":"display_size","width":2560,"height":1440}
//! ```
//!
//! A machine of the same account can ask another to update itself, when
//! the grid shows it on an older Dusk. `at` is when it asked, in Unix
//! seconds, and the proof signs it with the machine asked, so the request
//! cannot be sent again later or to another machine:
//!
//! ```text
//! connecting → host   {"type":"update","at":1792700000,"streaming":false,
//!                      "proof":{"account":1,"machine":"…","signature":"…"}}
//! host → connecting   {"type":"updating","version":"0.3.36"}  restarting into it
//!                     {"type":"up_to_date"}
//!                     {"type":"declined","reason":"..."}
//! ```
//!
//! `streaming` says the asking machine is streaming from the host, so the
//! stream that a restart interrupts is its own. A host streaming to anyone
//! else declines. The identity answer says which Dusk a machine runs, and
//! whether it can update this way, so the grid knows when to offer it.
//!
//! It is asked before every stream, whatever size the stream will be,
//! because a Mac answering it also wakes its screen.
//!
//! `screen` is the asking machine's own, and is new in 0.3.10. A host
//! streaming its virtual display answers with the size closest to it that
//! the display has, so the stream is the asker's shape; one streaming its
//! own monitor answers with that monitor's size. A Dusk that predates
//! `screen` ignores it. A Dusk too old to know the question at all
//! declines, and the stream falls back to a fixed size.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};

use crate::state::AppState;
use crate::sunshine::{self, SunshineApi};

/// Clear of Sunshine's own ports (47984–48010) and of anything common.
pub const PORT: u16 = 47850;

/// Long enough to walk to the other machine, short enough that a forgotten
/// request does not sit there indefinitely. Inside Moonlight's own pairing
/// timeout, so the client is still waiting when the answer comes.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(90);

/// How long the connecting side waits on each line of an answer: the most
/// a host that took the request can take over it. Checking the account
/// with GitHub and then offering the PIN for as long as the client may take
/// to send its own request is one path; asking the person and then the
/// same is the other, and the longer. Not shorter than either, because a
/// reply that times out is reported as the host having gone, and that
/// stops the client, which may have been seconds from pairing.
///
/// The client's own pairing timeout is what really bounds a pairing: the
/// ask is dropped the moment the client finishes, however it finishes.
const REPLY_TIMEOUT: Duration =
    Duration::from_secs(ANSWER_TIMEOUT.as_secs() + PIN_WINDOW.as_secs() + 10);

/// How long the connecting side waits on a reply to a pairing by
/// certificate. The host checks the account with GitHub, writes its file
/// and may restart before it replies, which takes up to about forty
/// seconds; a host that takes the request and says nothing for longer
/// than that is not going to, and the PIN path is waiting behind this.
const CERT_REPLY_TIMEOUT: Duration = Duration::from_secs(90);

/// The longest line read from a peer, 16 KiB. A request is small, but a
/// certificate pairing carries PEM certificates and a signature; some reads
/// allow a multiple of this.
const MAX_LINE: usize = 16 * 1024;

/// What a Dusk says to a request it does not know, as every version has.
const UNDERSTOOD_NOTHING: &str = "Dusk did not understand that request.";

pub const PAIR_REQUEST_EVENT: &str = "dusk://pair-request";
pub const PAIR_REQUEST_ENDED_EVENT: &str = "dusk://pair-request-ended";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Pair {
        pin: String,
        name: String,
        /// Present when the asking machine is signed in: its signature over
        /// the PIN, which lets a machine on the same account skip the prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof: Option<crate::account::Proof>,
    },
    /// Pair by certificate, with no PIN: the asking machine's certificate and
    /// a signature proving it speaks for the account. Only ever sent signed in.
    PairCert {
        name: String,
        cert: String,
        proof: crate::account::Proof,
    },
    /// The host's side of a certificate pairing: its own certificate, the id
    /// to pair under, whether it is ready to be streamed from, and its proof.
    Paired {
        server_cert: String,
        uniqueid: String,
        ready: bool,
        proof: crate::account::Proof,
    },
    Waiting,
    Accepted,
    Declined {
        reason: String,
    },
    /// What size is the screen being streamed? Nothing secret: it is the
    /// size every stream from this machine already shows.
    Display {
        /// The asking machine's screen, in real pixels.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<Size>,
    },
    DisplaySize {
        width: u32,
        height: u32,
        /// How many screens a stream from here can be switched between.
        /// New in 0.3.40; older ones say nothing, and the overlay then
        /// cannot know.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        displays: Option<u32>,
    },
    /// Which machine is this?
    ///
    /// Asked so the grid can be the machines on your account rather than
    /// whatever is advertising on the network. The account's machine list is
    /// keyed by machine id and carries no addresses, so something has to
    /// join the two, and a name will not do it: this Mac is `Michaels-Mini`
    /// on the account and advertises itself over mDNS as
    /// `Michaels-Mac-mini.local.`. The id is the key the list is already
    /// built on.
    Identify,
    /// Nothing secret. The id's public half is already published to the
    /// account's machine list, and the name is what the machine calls itself
    /// on the network. A machine that is signed out has no id and says so,
    /// which reads as "not one of yours" rather than as an error.
    Identity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
        name: String,
        /// This machine's Dusk version. New in 0.3.36; older ones say nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        /// It can update itself with nobody at it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        remote_update: bool,
    },
    /// Update yourself: asked by another machine on the same account.
    Update {
        at: u64,
        /// The asking machine is streaming from this one.
        #[serde(default)]
        streaming: bool,
        proof: crate::account::Proof,
    },
    /// A newer release is going in, and Dusk restarts into it.
    Updating {
        version: String,
    },
    UpToDate,
}

/// How far apart the two machines' clocks, and the request's trip, may be
/// before an update request counts as stale.
const UPDATE_REQUEST_LIFETIME: u64 = 5 * 60;

/// What another machine's Dusk said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identified {
    pub machine: Option<String>,
    pub name: String,
    pub dusk: Option<crate::model::DuskInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// What the host's window is asked.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub id: u64,
    pub name: String,
    pub address: String,
}

/// The one request waiting on an answer, if any.
///
/// One at a time on purpose: two machines asking at once is rare, and a
/// stack of prompts is how someone ends up allowing the wrong one.
#[derive(Default)]
pub struct Requests {
    next: AtomicU64,
    pending: Mutex<Option<(u64, oneshot::Sender<bool>)>>,
}

impl Requests {
    /// Deliver the person's answer. False when the request is already gone.
    pub async fn answer(&self, id: u64, allow: bool) -> bool {
        let mut pending = self.pending.lock().await;
        match pending.take() {
            Some((pending_id, tx)) if pending_id == id => tx.send(allow).is_ok(),
            other => {
                *pending = other;
                false
            }
        }
    }

    async fn open(&self) -> Option<(u64, oneshot::Receiver<bool>)> {
        let mut pending = self.pending.lock().await;
        if pending.as_ref().is_some_and(|(_, tx)| !tx.is_closed()) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        *pending = Some((id, tx));
        Some((id, rx))
    }

    async fn close(&self, id: u64) {
        let mut pending = self.pending.lock().await;
        if pending.as_ref().is_some_and(|(p, _)| *p == id) {
            *pending = None;
        }
    }
}

// --------------------------------------------------------------------- host

/// Listen for other Dusks. A port already taken costs zero-click pairing on
/// this machine, and nothing else: the PIN still works.
pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("dusk: not listening for other Dusks on {PORT}: {err}");
                return;
            }
        };
        loop {
            let Ok((stream, peer)) = listener.accept().await else {
                continue;
            };
            let app = app.clone();
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                serve(app, state, stream, peer.ip().to_string()).await;
            });
        }
    });
}

async fn serve(app: AppHandle, state: Arc<AppState>, stream: TcpStream, address: String) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read.take(MAX_LINE as u64 * 2)).lines();

    let first = tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await;
    let Ok(Ok(Some(line))) = first else {
        return;
    };
    let reply = match serde_json::from_str::<Message>(&line) {
        Ok(Message::Pair { pin, name, proof }) if valid_pin(&pin) => {
            let name = clean_name(&name).unwrap_or_else(|| address.clone());
            // A dropped connection means the asking side gave up; the
            // prompt should go with it.
            let gone = async { while let Ok(Some(_)) = lines.next_line().await {} };
            let strangers_welcome = state.store.lock().await.allow_other_devices
                || state.credentials.load().await.is_none();
            match pair_for_account(&state, &address, &pin, &name, proof.as_ref()).await {
                Answer::Replied(reply) => reply,
                // A host that is not set up yet gets past this, so that
                // `handle_pair` can say that instead, which is the real reason.
                Answer::NotVouched | Answer::CouldNotCheck(_) if strangers_welcome => {
                    handle_pair(&app, &state, &mut write, &address, &pin, &name, gone).await
                }
                Answer::CouldNotCheck(reason) => Message::Declined {
                    reason: could_not_check(&reason),
                },
                Answer::NotVouched => Message::Declined {
                    reason: not_on_this_account(proof.is_some()),
                },
            }
        }
        Ok(Message::PairCert { name, cert, proof }) => {
            let name = clean_name(&name).unwrap_or_else(|| address.clone());
            crate::cert_pairing::host::handle(&state, &address, name, cert, proof).await
        }
        Ok(Message::Identify) => Message::Identity {
            machine: state.account.machine_id().await,
            name: host_name(),
            version: Some(app.package_info().version.to_string()),
            remote_update: crate::updates::unattended::unattended()
                == crate::updates::unattended::Unattended::Possible,
        },
        Ok(Message::Update {
            at,
            streaming,
            proof,
        }) => handle_update(&app, &state, at, streaming, &proof).await,
        Ok(Message::Display { screen }) => {
            wake_screen();
            match display_answer(&app, &state, screen).await {
                Some((width, height)) => Message::DisplaySize {
                    width,
                    height,
                    displays: host_display_count(&app, &state).await,
                },
                None => Message::Declined {
                    reason: "This machine could not tell its screen size.".into(),
                },
            }
        }
        _ => Message::Declined {
            reason: UNDERSTOOD_NOTHING.into(),
        },
    };
    let _ = send(&mut write, &reply).await;
}

/// Update this machine, because another on its account asked.
async fn handle_update(
    app: &AppHandle,
    state: &Arc<AppState>,
    at: u64,
    streaming: bool,
    proof: &crate::account::Proof,
) -> Message {
    use crate::updates::unattended::{self, Found};
    let declined = |reason: String| Message::Declined { reason };
    let name = host_name();
    let Some(me) = state.account.machine_id().await else {
        return declined(not_on_this_account(true));
    };
    let now = crate::account::github::now_unix();
    if !fresh(at, now) {
        return declined(format!(
            "The request reached {name} too late, or the two machines' clocks disagree."
        ));
    }
    let message = crate::account::keys::update_message(&me, at, streaming);
    match state.account.vouch_for(&state.http, proof, &message).await {
        crate::account::Vouch::Vouched => {}
        crate::account::Vouch::NotOnAccount => return declined(not_on_this_account(true)),
        crate::account::Vouch::CouldNotCheck(reason) => return declined(could_not_check(&reason)),
    }
    // After the signature, so only a request that would otherwise be acted
    // on is remembered.
    if !UPDATE_REQUESTS_SEEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first_time(&format!("{}\n{at}\n{streaming}", proof.machine), at, now)
    {
        return declined("That request was already used.".into());
    }
    if let Some(why) = unattended::cannot_update(unattended::unattended(), &name) {
        return declined(why);
    }
    if let Some(why) = unattended::busy(state, streaming).await {
        return declined(why);
    }
    match unattended::update_now(app, state, streaming).await {
        Ok(Found::Installing { version }) => Message::Updating { version },
        Ok(Found::UpToDate) => Message::UpToDate,
        Err(reason) => declined(format!("{name} could not update: {reason}")),
    }
}

/// Update requests acted on recently, so each works once: the peer port is
/// plain TCP, and a request seen on the network must not be sendable again
/// inside its lifetime. Kept by what was signed and who signed it, not by
/// the signature's text, which more than one spelling of the same hex
/// would get past.
static UPDATE_REQUESTS_SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Seen(Vec::new()));

struct Seen(Vec<(String, u64)>);

impl Seen {
    /// Whether the request `request`, made at `at`, is new, remembering it
    /// if so. Forgets requests too old to be [`fresh`] any more.
    fn first_time(&mut self, request: &str, at: u64, now: u64) -> bool {
        self.0.retain(|(_, seen_at)| fresh(*seen_at, now));
        if self.0.iter().any(|(r, _)| r == request) {
            return false;
        }
        self.0.push((request.to_string(), at));
        true
    }
}

/// Whether an update request made at `at` is recent enough to act on.
fn fresh(at: u64, now: u64) -> bool {
    at.abs_diff(now) <= UPDATE_REQUEST_LIFETIME
}

/// The size a stream from here should be, for a machine whose screen is
/// `screen`.
///
/// Sizing a stream to whoever is watching, it is the asker's own screen as
/// near as this machine can give it — which is exactly, on a host that can
/// scale, and the closest size the virtual display has otherwise, since a
/// size it does not have would make the host give up on the virtual display
/// and stream the monitor. An asker too old to say its size is treated as
/// this machine's size.
async fn display_answer(
    app: &AppHandle,
    state: &Arc<AppState>,
    screen: Option<Size>,
) -> Option<(u32, u32)> {
    let own = host_display(app);
    if !state.store.lock().await.virtual_screen {
        return own;
    }
    let want = screen.map(|s| (s.width, s.height)).or(own)?;
    let offer = crate::host::virtual_screen::offer_for(want);
    #[cfg(target_os = "linux")]
    if let Some(size) = offer {
        match_screen_for_a_resumed_stream(state, size).await;
    }
    offer.or(own)
}

/// Resize the screen now if the stream about to start will resume a
/// session nobody is watching.
///
/// The screen is matched to the client by the host's `do` hook, and the host
/// runs that only when it launches an app. A client that dropped instead of
/// quitting leaves its app running, and every later stream resumes it — at
/// the size it asks for, captured from a screen still at the host's own
/// size. This is the one moment the host's Dusk hears of such a stream
/// before it starts, so the switch is made here. Nothing changes while a
/// client is still connected: someone is watching that screen.
///
/// `undo` puts it back when the resumed session is quit, and
/// [`watch_for_a_screen_left_resized`] when it is dropped again.
#[cfg(target_os = "linux")]
async fn match_screen_for_a_resumed_stream(state: &Arc<AppState>, size: (u32, u32)) {
    let Some(credentials) = state.credentials.load().await else {
        return;
    };
    if !crate::cert_pairing::reconcile::session_left_open(&state.http, &credentials).await {
        return;
    }
    let switched =
        tauri::async_runtime::spawn_blocking(move || crate::host::screen_mode::switch_to(size))
            .await;
    match switched {
        Ok(Ok(())) => eprintln!(
            "dusk: matched the screen to {}x{} for a stream resuming a session",
            size.0, size.1
        ),
        Ok(Err(err)) => eprintln!("dusk: the screen was left as it was: {err}"),
        Err(err) => eprintln!("dusk: the screen was left as it was: {err}"),
    }
}

/// Put the screen back once a stream that resized it has nobody watching.
///
/// `undo` comes with the end of the session, and a client that drops
/// rather than quits leaves the session up — so without this the host's
/// screen would stay at the client's size until Dusk next started. Two
/// checks a minute apart, both finding no client, before anything is done:
/// a stream that is just starting has its screen switched by `do` a moment
/// before its client shows up in the log.
#[cfg(target_os = "linux")]
pub fn watch_for_a_screen_left_resized(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut unwatched = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if !crate::host::screen_mode::resized() {
                unwatched = 0;
                continue;
            }
            let left_open = match state.credentials.load().await {
                Some(credentials) => {
                    crate::cert_pairing::reconcile::session_left_open(&state.http, &credentials)
                        .await
                }
                None => false,
            };
            unwatched = if left_open { unwatched + 1 } else { 0 };
            if unwatched >= 2 {
                unwatched = 0;
                let _ =
                    tauri::async_runtime::spawn_blocking(crate::host::screen_mode::put_back).await;
            }
        }
    });
}

/// Turn this machine's screen on, because a stream from it is about to
/// start.
///
/// A Mac whose display has gone to sleep streams black, and is slow to
/// notice anyone connecting; one that is locked shows its lock screen,
/// which the person streaming can type past. Declaring the user active is
/// what touching a key would do. Held long enough for the stream to start,
/// after which its input keeps the screen on. Only a Mac is known to need
/// it.
fn wake_screen() {
    #[cfg(target_os = "macos")]
    std::thread::spawn(|| {
        let _ = std::process::Command::new("/usr/bin/caffeinate")
            .args(["-u", "-t", "30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// The size of the screen a stream from here shows, in real pixels.
///
/// The primary screen, because that is the one the host streams unless
/// someone has picked another. Wayland may not name a primary, so the first
/// screen stands in for it there.
fn host_display(app: &AppHandle) -> Option<(u32, u32)> {
    let monitor = match app.primary_monitor() {
        Ok(Some(monitor)) => monitor,
        _ => app.available_monitors().ok()?.into_iter().next()?,
    };
    let size = monitor.size();
    (size.width > 0 && size.height > 0).then_some((size.width, size.height))
}

/// How many screens the host can switch a stream between.
///
/// Every screen the desktop has, since that is what the host lists for its
/// display switch, except while the virtual display is on: Windows then
/// turns the other screens off for the length of a stream, leaving the
/// virtual one as the only one there is. `None` when the desktop will not
/// say.
async fn host_display_count(app: &AppHandle, state: &Arc<AppState>) -> Option<u32> {
    if cfg!(windows) && state.store.lock().await.virtual_screen {
        return Some(1);
    }
    let count = app.available_monitors().ok()?.len();
    (count > 0).then_some(count as u32)
}

/// How [`pair_for_account`] went.
enum Answer {
    /// The request was one of this account's; this is the reply.
    Replied(Message),
    /// Not on this account, or no proof at all. Ask as usual.
    NotVouched,
    /// Could not find out whether it is on this account; the reason is
    /// worded for the person at the other machine.
    CouldNotCheck(String),
}

/// Pair without asking, when the request is signed by another machine on
/// this machine's own account: the question the prompt would ask was
/// answered when both were signed in.
async fn pair_for_account(
    state: &Arc<AppState>,
    address: &str,
    pin: &str,
    name: &str,
    proof: Option<&crate::account::Proof>,
) -> Answer {
    let Some(proof) = proof else {
        return Answer::NotVouched;
    };
    let Some(credentials) = state.credentials.load().await else {
        return Answer::NotVouched;
    };
    match state
        .account
        .vouch_for(&state.http, proof, &crate::account::keys::pair_message(pin))
        .await
    {
        crate::account::Vouch::Vouched => {}
        crate::account::Vouch::NotOnAccount => return Answer::NotVouched,
        crate::account::Vouch::CouldNotCheck(reason) => return Answer::CouldNotCheck(reason),
    }
    eprintln!("dusk: {name} is on this account; pairing without asking");
    Answer::Replied(
        match enter_pin(&credentials, Some(address), pin, name).await {
            Ok(()) => Message::Accepted,
            Err(err) => Message::Declined {
                reason: err.to_string(),
            },
        },
    )
}

/// Why a signed request could not be checked against this machine's
/// account, said to the person at the machine that asked. Not the same
/// as [`not_on_this_account`]: signing in again *there* would not help.
fn could_not_check(reason: &str) -> String {
    let host = host_name();
    let reason = reason.trim_end_matches('.');
    format!(
        "{host} couldn't check whether this machine is on its GitHub account ({reason}). \
         Sign in again on {host}, or turn on Let other devices pair there in Settings › Hosting."
    )
}

async fn handle_pair(
    app: &AppHandle,
    state: &Arc<AppState>,
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    address: &str,
    pin: &str,
    name: &str,
    gone: impl std::future::Future<Output = ()>,
) -> Message {
    let Some(credentials) = state.credentials.load().await else {
        return Message::Declined {
            reason: "That machine is not set up to host yet.".into(),
        };
    };
    let Some((id, answer)) = state.peer.open().await else {
        return Message::Declined {
            reason: "That machine is already answering another request.".into(),
        };
    };

    let request = PairRequest {
        id,
        name: name.to_string(),
        address: address.to_string(),
    };
    let _ = app.emit(PAIR_REQUEST_EVENT, &request);
    crate::tray::show_window(app);
    if send(write, &Message::Waiting).await.is_err() {
        state.peer.close(id).await;
        let _ = app.emit(PAIR_REQUEST_ENDED_EVENT, id);
        return Message::Declined {
            reason: String::new(),
        };
    }

    let allowed = tokio::select! {
        answer = answer => answer.unwrap_or(false),
        _ = tokio::time::sleep(ANSWER_TIMEOUT) => false,
        _ = gone => false,
    };
    state.peer.close(id).await;
    let _ = app.emit(PAIR_REQUEST_ENDED_EVENT, id);

    if !allowed {
        return Message::Declined {
            reason: format!("Nobody allowed this on {}.", host_name()),
        };
    }

    match enter_pin(&credentials, Some(address), pin, name).await {
        Ok(()) => Message::Accepted,
        Err(err) => Message::Declined {
            reason: err.to_string(),
        },
    }
}

/// Why a machine outside this one's account was turned away, said to the
/// person at the machine that asked. `signed_in` is whether it sent a proof.
fn not_on_this_account(signed_in: bool) -> String {
    let host = host_name();
    if signed_in {
        format!(
            "{host} is not signed in to the same GitHub account as this machine. \
             Sign in to that account on {host} too, in Settings › Account."
        )
    } else {
        format!(
            "Sign in to GitHub in Settings › Account, with the same account as \
             {host}, and the two will pair on their own."
        )
    }
}

/// How long the host keeps offering the PIN while the client gets ready.
///
/// This was five seconds, and five seconds is not enough. The client is a
/// Qt application that has to start, initialise, resolve the host and send
/// its pairing request, and [`crate::moonlight::cli`] allows it two minutes
/// to do all that. Sunshine refuses a PIN nobody is waiting on yet, so the
/// host would exhaust its five attempts, report "that PIN was not accepted",
/// and — because declining makes the connecting side return — *kill the
/// client it was waiting for*. A pairing that would have worked was aborted
/// by the machine that was supposed to be completing it.
///
/// Thirty seconds was the first attempt at this and was still too short.
/// Measured on a machine with several virtual interfaces, a cold client
/// spends minutes before it pairs: it wakes the host on every address it can
/// think of — around fifty packets across ZeroTier, Tailscale, two LAN
/// addresses and IPv6 multicast — then browses mDNS, then asks for
/// `serverinfo`. Only then does it pair. The tell was that the *second*
/// attempt always worked, because by then it was warm.
///
/// So this is not a number of its own any more. The host waits as long as
/// the client is allowed to take, less a moment to report the failure
/// before the client gives up on its own. Any value picked independently is
/// a value that can drift back under the client's.
const PIN_WINDOW: Duration =
    Duration::from_secs(crate::moonlight::cli::PAIR_TIMEOUT.as_secs() - 10);

/// Enter the PIN into Sunshine, giving the client's own pairing request time
/// to arrive: Sunshine refuses a PIN nobody is waiting on, and the two
/// travel separately.
///
/// Sunshine answers the same way for "no client is waiting yet" and "that
/// PIN is wrong", so there is nothing to tell them apart by and no way to
/// fail faster on a genuinely wrong PIN. Waiting is the right trade: a wrong
/// PIN costs thirty seconds before it is reported, where giving up early
/// cost every pairing on a machine whose client is slow to start.
///
/// `address` is where the request came from, when a peer carried it; a
/// PIN typed in by hand has none, and is offered to every waiting pairing.
pub(crate) async fn enter_pin(
    credentials: &sunshine::Credentials,
    address: Option<&str>,
    pin: &str,
    name: &str,
) -> Result<(), sunshine::ApiError> {
    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT)?;
    let deadline = tokio::time::Instant::now() + PIN_WINDOW;
    let mut last = None;
    while tokio::time::Instant::now() < deadline {
        match api.submit_pin(credentials, pin, name, address).await {
            Ok(()) => return Ok(()),
            Err(sunshine::ApiError::Unauthorized) => return Err(sunshine::ApiError::Unauthorized),
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(last.unwrap_or(sunshine::ApiError::Failed(
        "The host did not take the PIN.".into(),
    )))
}

// --------------------------------------------------------------- connecting

/// How asking another Dusk went.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No Dusk there. Fall back to showing the PIN.
    NoPeer,
    Accepted,
    Declined(String),
}

/// Ask the Dusk at `address` to enter `pin`. `on_waiting` runs once someone
/// on the other end is being asked, so the UI can say so.
pub async fn ask(
    address: &str,
    pin: &str,
    proof: Option<crate::account::Proof>,
    on_waiting: impl FnOnce(),
) -> Outcome {
    ask_at(address, PORT, pin, proof, on_waiting).await
}

async fn ask_at(
    address: &str,
    port: u16,
    pin: &str,
    proof: Option<crate::account::Proof>,
    on_waiting: impl FnOnce(),
) -> Outcome {
    let connect = TcpStream::connect((address, port));
    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), connect).await else {
        return Outcome::NoPeer;
    };
    let (read, mut write) = stream.into_split();
    let request = Message::Pair {
        pin: pin.to_string(),
        name: host_name(),
        proof,
    };
    if send(&mut write, &request).await.is_err() {
        return Outcome::NoPeer;
    }

    let mut lines = BufReader::new(read.take(MAX_LINE as u64 * 4)).lines();
    let mut on_waiting = Some(on_waiting);
    loop {
        let next = tokio::time::timeout(REPLY_TIMEOUT, lines.next_line());
        let line = match next.await {
            Ok(Ok(Some(line))) => line,
            // Something answered on the port and then went away without a
            // verdict. Not a Dusk worth waiting on.
            _ if on_waiting.is_some() => return Outcome::NoPeer,
            _ => return Outcome::Declined("The other machine stopped answering.".into()),
        };
        match serde_json::from_str::<Message>(&line) {
            Ok(Message::Waiting) => {
                if let Some(f) = on_waiting.take() {
                    f();
                }
            }
            Ok(Message::Accepted) => return Outcome::Accepted,
            Ok(Message::Declined { reason }) => return Outcome::Declined(reason),
            _ if on_waiting.is_some() => return Outcome::NoPeer,
            _ => return Outcome::Declined("The other machine answered something odd.".into()),
        }
    }
}

/// How asking another Dusk to pair by certificate went.
#[derive(Debug, PartialEq, Eq)]
pub enum CertOutcome {
    /// No Dusk there, or one that could not take the request. Fall back to
    /// the PIN.
    NoPeer,
    /// The host trusts this machine's certificate now. `ready` is false
    /// while it waits for a stream to end before it can stream to us.
    Paired {
        server_cert: String,
        uniqueid: String,
        ready: bool,
        proof: crate::account::Proof,
    },
    Declined(String),
}

/// Ask the Dusk at `address` to pair by certificate, with `request` a
/// [`Message::PairCert`].
///
/// Only a refusal the host put into words is `Declined`. Everything else is
/// `NoPeer`: a Dusk too old to know the request reads only part of a line
/// this long and may reset the connection before it says anything.
pub async fn ask_cert(address: &str, request: Message) -> CertOutcome {
    ask_cert_at(address, PORT, request).await
}

async fn ask_cert_at(address: &str, port: u16, request: Message) -> CertOutcome {
    let connect = TcpStream::connect((address, port));
    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), connect).await else {
        return CertOutcome::NoPeer;
    };
    let (read, mut write) = stream.into_split();
    if send(&mut write, &request).await.is_err() {
        return CertOutcome::NoPeer;
    }

    let mut lines = BufReader::new(read.take(MAX_LINE as u64 * 4)).lines();
    let Ok(Ok(Some(line))) = tokio::time::timeout(CERT_REPLY_TIMEOUT, lines.next_line()).await
    else {
        return CertOutcome::NoPeer;
    };
    match serde_json::from_str::<Message>(&line) {
        Ok(Message::Paired {
            server_cert,
            uniqueid,
            ready,
            proof,
        }) => CertOutcome::Paired {
            server_cert,
            uniqueid,
            ready,
            proof,
        },
        Ok(Message::Declined { reason }) => CertOutcome::Declined(reason),
        _ => CertOutcome::NoPeer,
    }
}

/// What a host says about the screens a stream from it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostScreen {
    /// The size to stream at.
    pub size: (u32, u32),
    /// How many screens the stream can be switched between, if it said.
    pub displays: Option<u32>,
}

/// Ask the Dusk at `address` what size to stream at, telling it the size of
/// the screen here. `None` when there is no Dusk there, or one too old to
/// say. Kept short: a stream is waiting.
pub async fn display_size(address: &str, screen: Option<(u32, u32)>) -> Option<HostScreen> {
    display_size_at(address, PORT, screen).await
}

async fn display_size_at(
    address: &str,
    port: u16,
    screen: Option<(u32, u32)>,
) -> Option<HostScreen> {
    let budget = Duration::from_secs(2);
    let connect = TcpStream::connect((address, port));
    let stream = tokio::time::timeout(budget, connect).await.ok()?.ok()?;
    let (read, mut write) = stream.into_split();
    let screen = screen.map(|(width, height)| Size { width, height });
    send(&mut write, &Message::Display { screen }).await.ok()?;
    let mut lines = BufReader::new(read.take(MAX_LINE as u64)).lines();
    let line = tokio::time::timeout(budget, lines.next_line())
        .await
        .ok()?
        .ok()??;
    match serde_json::from_str::<Message>(&line).ok()? {
        // Nothing streams at a size like 0x0 or 100000 wide; a host saying
        // so is broken, and a fixed size serves better than believing it.
        Message::DisplaySize {
            width,
            height,
            displays,
        } if (320..=15360).contains(&width) && (240..=8640).contains(&height) => {
            Some(HostScreen {
                size: (width, height),
                // Sunshine switches on F1..F12, so a count past that, or
                // none at all, is not one to offer.
                displays: displays.filter(|n| (1..=12).contains(n)),
            })
        }
        _ => None,
    }
}

/// Who is the Dusk at `address`?
///
/// `None` when nothing answers, or when a Dusk too old to know the question
/// does. That is not a failure: a machine Dusk cannot identify is simply one
/// the grid cannot claim as yours, which is the honest answer rather than a
/// guess from its name.
pub async fn identify(address: &str) -> Option<Identified> {
    identify_at(address, PORT).await
}

async fn identify_at(address: &str, port: u16) -> Option<Identified> {
    // Asked on the poll tick for every machine, so it must never be the slow
    // part of one. A machine that does not answer in two seconds is one the
    // grid shows as it did before.
    let budget = Duration::from_secs(2);
    let connect = TcpStream::connect((address, port));
    let stream = tokio::time::timeout(budget, connect).await.ok()?.ok()?;
    let (read, mut write) = stream.into_split();
    send(&mut write, &Message::Identify).await.ok()?;
    let mut lines = BufReader::new(read.take(MAX_LINE as u64)).lines();
    let line = tokio::time::timeout(budget, lines.next_line())
        .await
        .ok()?
        .ok()??;
    match serde_json::from_str::<Message>(&line).ok()? {
        Message::Identity {
            machine,
            name,
            version,
            remote_update,
        } => Some(Identified {
            machine,
            name,
            dusk: version.map(|version| crate::model::DuskInfo {
                version,
                remote_update,
            }),
        }),
        _ => None,
    }
}

/// What asking another machine to update found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UpdateOutcome {
    Updating { version: String },
    UpToDate,
}

/// Ask the machine at `address`, whose account id is `machine`, to update.
pub async fn ask_update(
    state: &AppState,
    address: &str,
    machine: &str,
    streaming: bool,
) -> Result<UpdateOutcome, String> {
    let at = crate::account::github::now_unix();
    let proof = state
        .account
        .sign(&crate::account::keys::update_message(
            machine, at, streaming,
        ))
        .await
        .ok_or("Sign in to GitHub on this machine to update your others from it.")?;
    ask_update_at(
        address,
        PORT,
        &Message::Update {
            at,
            streaming,
            proof,
        },
    )
    .await
}

async fn ask_update_at(
    address: &str,
    port: u16,
    request: &Message,
) -> Result<UpdateOutcome, String> {
    let gone = || "That machine didn't answer.".to_string();
    let connect = TcpStream::connect((address, port));
    let stream = tokio::time::timeout(Duration::from_secs(5), connect)
        .await
        .map_err(|_| gone())?
        .map_err(|_| gone())?;
    let (read, mut write) = stream.into_split();
    send(&mut write, request).await.map_err(|_| gone())?;
    let mut lines = BufReader::new(read.take(MAX_LINE as u64)).lines();
    // The host checks the account, and then GitHub for a release, before it
    // answers.
    let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
        .await
        .map_err(|_| gone())?
        .map_err(|_| gone())?
        .ok_or_else(gone)?;
    match serde_json::from_str::<Message>(&line).map_err(|_| gone())? {
        Message::Updating { version } => Ok(UpdateOutcome::Updating { version }),
        Message::UpToDate => Ok(UpdateOutcome::UpToDate),
        Message::Declined { reason } if reason == UNDERSTOOD_NOTHING => {
            Err("That machine's Dusk is too old to be updated from here.".into())
        }
        Message::Declined { reason } => Err(reason),
        _ => Err(gone()),
    }
}

// ------------------------------------------------------------------ helpers

/// The host part of an address as the grid stores it, which may carry
/// Sunshine's port: `10.0.0.5:47989`, `[fe80::1]:47989`, or bare.
pub fn host_part(address: &str) -> &str {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match address.matches(':').count() {
        1 => address.split(':').next().unwrap_or(address),
        // Bare IPv6 has several colons and no port to strip.
        _ => address,
    }
}

async fn send(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    message: &Message,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 4 && pin.bytes().all(|b| b.is_ascii_digit())
}

/// A machine name fit to put in front of someone: printable, trimmed, short.
fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

pub fn host_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim_end_matches(".local")
        .to_string()
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn the_host_waits_long_enough_for_a_client_that_is_slow_to_start() {
        // The bug this replaces: the host gave up on the PIN after five
        // seconds while the client had two minutes to send its request, so
        // the host aborted pairings that were still getting under way.
        // Whatever these numbers become, the host must not be the first to
        // give up.
        // Measured: a cold client on a machine with several virtual
        // interfaces took minutes to get as far as pairing, and only the
        // second attempt ever worked because it was warm by then.
        assert!(
            PIN_WINDOW >= Duration::from_secs(60),
            "a cold client spends a long time discovering before it pairs"
        );
        assert!(
            PIN_WINDOW < crate::moonlight::cli::PAIR_TIMEOUT,
            "the client, not the host, decides when pairing has failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_a_proof_still_parses_and_one_with_it_round_trips() {
        let old = r#"{"type":"pair","pin":"0042","name":"Mac mini"}"#;
        assert!(matches!(
            serde_json::from_str::<Message>(old).unwrap(),
            Message::Pair { proof: None, .. }
        ));
        let signed = Message::Pair {
            pin: "0042".into(),
            name: "Mac mini".into(),
            proof: Some(crate::account::Proof {
                account: 1,
                machine: "ab".into(),
                signature: "cd".into(),
            }),
        };
        let line = serde_json::to_string(&signed).unwrap();
        assert!(line.len() < MAX_LINE);
        assert_eq!(serde_json::from_str::<Message>(&line).unwrap(), signed);
    }

    #[test]
    fn the_wire_format_is_the_one_documented() {
        let pair = Message::Pair {
            pin: "0042".into(),
            name: "Mac mini".into(),
            proof: None,
        };
        assert_eq!(
            serde_json::to_string(&pair).unwrap(),
            r#"{"type":"pair","pin":"0042","name":"Mac mini"}"#
        );
        assert_eq!(
            serde_json::to_string(&Message::Waiting).unwrap(),
            r#"{"type":"waiting"}"#
        );
    }

    #[test]
    fn the_port_comes_off_an_address_and_ipv6_survives() {
        assert_eq!(host_part("10.0.0.5:47989"), "10.0.0.5");
        assert_eq!(host_part("10.0.0.5"), "10.0.0.5");
        assert_eq!(host_part("[fe80::1]:47989"), "fe80::1");
        assert_eq!(host_part("fe80::1"), "fe80::1");
        assert_eq!(host_part("gaming-pc.local"), "gaming-pc.local");
    }

    #[test]
    fn only_four_digits_are_a_pin() {
        assert!(valid_pin("0042"));
        assert!(!valid_pin("042"));
        assert!(!valid_pin("00420"));
        assert!(!valid_pin("00a2"));
    }

    #[test]
    fn a_name_is_made_safe_to_show() {
        assert_eq!(clean_name("  Mac\u{7}mini \n").as_deref(), Some("Macmini"));
        assert_eq!(clean_name("\n\t"), None);
        assert_eq!(clean_name(&"x".repeat(500)).unwrap().len(), 64);
    }

    #[tokio::test]
    async fn one_request_at_a_time_and_answers_reach_the_right_one() {
        let requests = Requests::default();
        let (id, rx) = requests.open().await.unwrap();
        assert!(requests.open().await.is_none(), "a second must wait");
        assert!(
            !requests.answer(id + 1, true).await,
            "a stale id is ignored"
        );
        assert!(requests.answer(id, true).await);
        assert!(rx.await.unwrap());
        assert!(requests.open().await.is_some(), "free again once answered");
    }

    #[tokio::test]
    async fn an_abandoned_request_does_not_block_the_next() {
        let requests = Requests::default();
        let (_, rx) = requests.open().await.unwrap();
        drop(rx);
        assert!(requests.open().await.is_some());
    }

    #[test]
    fn the_display_question_is_the_one_documented() {
        assert_eq!(
            serde_json::to_string(&Message::Display {
                screen: Some(Size {
                    width: 3440,
                    height: 1440
                })
            })
            .unwrap(),
            r#"{"type":"display","screen":{"width":3440,"height":1440}}"#
        );
        assert_eq!(
            serde_json::to_string(&Message::Display { screen: None }).unwrap(),
            r#"{"type":"display"}"#
        );
        assert_eq!(
            serde_json::to_string(&Message::DisplaySize {
                width: 2560,
                height: 1440,
                displays: None,
            })
            .unwrap(),
            r#"{"type":"display_size","width":2560,"height":1440}"#
        );
        assert_eq!(
            serde_json::to_string(&Message::DisplaySize {
                width: 2560,
                height: 1440,
                displays: Some(2),
            })
            .unwrap(),
            r#"{"type":"display_size","width":2560,"height":1440,"displays":2}"#
        );
    }

    #[test]
    fn a_question_from_an_older_dusk_still_reads() {
        assert_eq!(
            serde_json::from_str::<Message>(r#"{"type":"display"}"#).unwrap(),
            Message::Display { screen: None }
        );
    }

    #[test]
    fn an_older_dusk_reads_the_new_question_as_the_old_one() {
        // The message as 0.3.9 and earlier declared it.
        #[derive(Debug, PartialEq, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Old {
            Display,
        }
        let new = Message::Display {
            screen: Some(Size {
                width: 3440,
                height: 1440,
            }),
        };
        let line = serde_json::to_string(&new).unwrap();
        assert_eq!(serde_json::from_str::<Old>(&line).unwrap(), Old::Display);
    }

    fn sample_proof() -> crate::account::Proof {
        crate::account::Proof {
            account: 1,
            machine: "ab".repeat(16),
            signature: "cd".repeat(32),
        }
    }

    fn crlf_pem() -> String {
        let body =
            "MIIDXTCCAkWgAwIBAgIJAKoK/heBjcOuMA0GCSqGSIb3DQEBCwUAMEUxCzAJBgNV\r\n".repeat(30);
        format!("-----BEGIN CERTIFICATE-----\r\n{body}-----END CERTIFICATE-----\r\n")
    }

    #[test]
    fn a_pair_cert_line_with_a_crlf_certificate_fits_and_round_trips() {
        let cert = crlf_pem();
        assert!(cert.len() >= 2000);
        let ask = Message::PairCert {
            name: "Mac mini".into(),
            cert,
            proof: sample_proof(),
        };
        let line = serde_json::to_string(&ask).unwrap();
        assert!(line.len() < MAX_LINE);
        assert_eq!(serde_json::from_str::<Message>(&line).unwrap(), ask);

        let reply = Message::Paired {
            server_cert: crlf_pem(),
            uniqueid: "0123456789ABCDEF".into(),
            ready: true,
            proof: sample_proof(),
        };
        let line = serde_json::to_string(&reply).unwrap();
        assert!(line.len() < MAX_LINE);
        assert_eq!(serde_json::from_str::<Message>(&line).unwrap(), reply);
    }

    #[test]
    fn an_older_dusk_reads_pair_cert_as_something_it_declines() {
        // Every message as 0.3.x declared it; `serve` declines whatever
        // does not parse as one of these.
        #[derive(Debug, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[allow(dead_code)]
        enum Old {
            Pair {
                pin: String,
                name: String,
            },
            Waiting,
            Accepted,
            Declined {
                reason: String,
            },
            Display,
            DisplaySize {
                width: u32,
                height: u32,
            },
            Identify,
            Identity {
                machine: Option<String>,
                name: String,
            },
        }
        let line = serde_json::to_string(&Message::PairCert {
            name: "Mac mini".into(),
            cert: crlf_pem(),
            proof: sample_proof(),
        })
        .unwrap();
        assert!(serde_json::from_str::<Old>(&line).is_err());
    }

    #[tokio::test]
    async fn a_host_too_old_for_pair_cert_declines_over_the_wire() {
        let port = host_expecting(
            |m| matches!(m, Message::PairCert { .. }),
            r#"{"type":"declined","reason":"Dusk did not understand that request."}"#,
        )
        .await;
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (read, mut write) = stream.into_split();
        send(
            &mut write,
            &Message::PairCert {
                name: "Mac mini".into(),
                cert: crlf_pem(),
                proof: sample_proof(),
            },
        )
        .await
        .unwrap();
        let mut lines = BufReader::new(read.take(MAX_LINE as u64 * 4)).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Message>(&line).unwrap(),
            Message::Declined {
                reason: "Dusk did not understand that request.".into()
            }
        );
    }

    /// A stand-in host that answers one request with `reply`, checking that
    /// what it was asked matches `expect`.
    async fn host_expecting(expect: fn(&Message) -> bool, reply: &'static str) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let asked = serde_json::from_str::<Message>(&line).unwrap();
            assert!(expect(&asked), "host was asked {asked:?}");
            write.write_all(reply.as_bytes()).await.unwrap();
            write.write_all(b"\n").await.unwrap();
        });
        port
    }

    /// A stand-in host for the display question.
    async fn host_answering(reply: &'static str) -> u16 {
        host_expecting(|m| matches!(m, Message::Display { .. }), reply).await
    }

    /// A stand-in host for the identity question.
    async fn host_identifying(reply: &'static str) -> u16 {
        host_expecting(|m| matches!(m, Message::Identify), reply).await
    }

    #[tokio::test]
    async fn a_host_says_how_big_its_screen_is() {
        let port = host_answering(r#"{"type":"display_size","width":3440,"height":1440}"#).await;
        assert_eq!(
            display_size_at("127.0.0.1", port, Some((3440, 1440))).await,
            Some(HostScreen {
                size: (3440, 1440),
                displays: None
            }),
            "a host older than the display count still gives its size"
        );
    }

    #[tokio::test]
    async fn a_host_says_how_many_screens_it_has() {
        let port =
            host_answering(r#"{"type":"display_size","width":3440,"height":1440,"displays":2}"#)
                .await;
        assert_eq!(
            display_size_at("127.0.0.1", port, None).await,
            Some(HostScreen {
                size: (3440, 1440),
                displays: Some(2)
            })
        );
        // More than the host can switch between is not a count to offer.
        let port =
            host_answering(r#"{"type":"display_size","width":3440,"height":1440,"displays":40}"#)
                .await;
        assert_eq!(
            display_size_at("127.0.0.1", port, None)
                .await
                .and_then(|h| h.displays),
            None
        );
    }

    #[tokio::test]
    async fn a_machine_says_which_one_it_is() {
        // The join key for "is this one of mine": the account's machine list
        // is keyed by this id and carries no addresses, so the grid cannot
        // match a discovered host to it any other way.
        let port = host_identifying(
            r#"{"type":"identity","machine":"8717645a43edf635","name":"Michaels-Mini"}"#,
        )
        .await;
        assert_eq!(
            identify_at("127.0.0.1", port).await,
            Some(Identified {
                machine: Some("8717645a43edf635".into()),
                name: "Michaels-Mini".into(),
                dusk: None,
            })
        );
    }

    #[tokio::test]
    async fn a_machine_says_which_dusk_it_runs() {
        let port = host_identifying(
            r#"{"type":"identity","machine":"aa","name":"Mini","version":"0.3.36","remote_update":true}"#,
        )
        .await;
        assert_eq!(
            identify_at("127.0.0.1", port).await.unwrap().dusk,
            Some(crate::model::DuskInfo {
                version: "0.3.36".into(),
                remote_update: true,
            })
        );
    }

    #[test]
    fn an_update_request_is_only_good_for_a_few_minutes() {
        assert!(fresh(1_000, 1_000));
        assert!(fresh(1_000, 1_000 + UPDATE_REQUEST_LIFETIME));
        // A clock a little ahead of this one's is not a replay.
        assert!(fresh(1_000 + 60, 1_000));
        assert!(!fresh(1_000, 1_000 + UPDATE_REQUEST_LIFETIME + 1));
        assert!(!fresh(1_000 + UPDATE_REQUEST_LIFETIME + 1, 1_000));
    }

    #[test]
    fn an_update_request_works_once() {
        let mut seen = Seen(Vec::new());
        assert!(seen.first_time("sig", 1_000, 1_000));
        assert!(!seen.first_time("sig", 1_000, 1_010), "sent again");
        assert!(seen.first_time("other", 1_005, 1_010));
        // Once the first is too old to be fresh it is forgotten, which is
        // safe: it would be refused as stale anyway.
        seen.first_time("later", 2_000, 2_000);
        assert_eq!(seen.0.len(), 1);
    }

    #[test]
    fn an_update_request_round_trips() {
        let request = Message::Update {
            at: 1_792_700_000,
            streaming: true,
            proof: crate::account::Proof {
                account: 1,
                machine: "ab".into(),
                signature: "cd".into(),
            },
        };
        let line = serde_json::to_string(&request).unwrap();
        assert!(line.starts_with(r#"{"type":"update","at":1792700000"#));
        assert_eq!(serde_json::from_str::<Message>(&line).unwrap(), request);
        assert_eq!(
            serde_json::to_string(&Message::UpToDate).unwrap(),
            r#"{"type":"up_to_date"}"#
        );
    }

    async fn host_replying_to_update(reply: &'static str) -> u16 {
        host_expecting(|m| matches!(m, Message::Update { .. }), reply).await
    }

    fn update_request() -> Message {
        Message::Update {
            at: 0,
            streaming: false,
            proof: crate::account::Proof {
                account: 1,
                machine: "ab".into(),
                signature: "cd".into(),
            },
        }
    }

    #[tokio::test]
    async fn an_update_answer_reaches_the_asker() {
        let port = host_replying_to_update(r#"{"type":"updating","version":"0.3.36"}"#).await;
        assert_eq!(
            ask_update_at("127.0.0.1", port, &update_request()).await,
            Ok(UpdateOutcome::Updating {
                version: "0.3.36".into()
            })
        );
        let port = host_replying_to_update(r#"{"type":"declined","reason":"Not now."}"#).await;
        assert_eq!(
            ask_update_at("127.0.0.1", port, &update_request()).await,
            Err("Not now.".into())
        );
    }

    #[tokio::test]
    async fn a_dusk_too_old_to_update_from_here_says_so() {
        let port = host_replying_to_update(
            r#"{"type":"declined","reason":"Dusk did not understand that request."}"#,
        )
        .await;
        assert!(ask_update_at("127.0.0.1", port, &update_request())
            .await
            .unwrap_err()
            .contains("too old"));
    }

    #[tokio::test]
    async fn a_signed_out_machine_identifies_without_claiming_an_account() {
        // Answering with no id is the honest "not one of yours", and has to
        // stay distinguishable from no answer at all.
        let port = host_identifying(r#"{"type":"identity","name":"someone-elses-pc"}"#).await;
        assert_eq!(
            identify_at("127.0.0.1", port).await,
            Some(Identified {
                machine: None,
                name: "someone-elses-pc".into(),
                dusk: None,
            })
        );
    }

    #[tokio::test]
    async fn a_dusk_too_old_to_know_the_question_is_not_an_error() {
        // It declines, as it does for any message it does not recognise.
        // That must read as "cannot be claimed as yours", never as a fault:
        // the name is all such a machine can offer and names do not match
        // (this Mac is Michaels-Mini on the account, Michaels-Mac-mini on
        // the network).
        let port = host_identifying(
            r#"{"type":"declined","reason":"Dusk did not understand that request."}"#,
        )
        .await;
        assert_eq!(identify_at("127.0.0.1", port).await, None);
    }

    #[tokio::test]
    async fn an_older_dusk_or_a_nonsense_size_is_no_answer() {
        let port = host_answering(
            r#"{"type":"declined","reason":"Dusk did not understand that request."}"#,
        )
        .await;
        assert_eq!(
            display_size_at("127.0.0.1", port, Some((3440, 1440))).await,
            None
        );
        let port = host_answering(r#"{"type":"display_size","width":0,"height":1440}"#).await;
        assert_eq!(
            display_size_at("127.0.0.1", port, Some((3440, 1440))).await,
            None
        );
    }

    #[tokio::test]
    async fn nobody_listening_is_no_peer() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let outcome = ask_at("127.0.0.1", port, "1234", None, || {
            panic!("nobody to wait on")
        })
        .await;
        assert_eq!(outcome, Outcome::NoPeer);
    }

    #[tokio::test]
    async fn something_else_on_the_port_is_no_peer() {
        // Not a Dusk: answers with something that is not the protocol.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        });
        let outcome = ask_at("127.0.0.1", port, "1234", None, || {}).await;
        assert_eq!(outcome, Outcome::NoPeer);
    }

    #[tokio::test]
    async fn an_answer_travels_back_over_the_wire() {
        // A stand-in host on an ephemeral port, speaking the protocol.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            assert!(matches!(
                serde_json::from_str::<Message>(&line).unwrap(),
                Message::Pair { .. }
            ));
            send(&mut write, &Message::Waiting).await.unwrap();
            send(&mut write, &Message::Accepted).await.unwrap();
        });

        let mut waited = false;
        let outcome = ask_at("127.0.0.1", port, "1234", None, || waited = true).await;
        assert_eq!(outcome, Outcome::Accepted);
        assert!(waited);
    }

    fn pair_cert_request() -> Message {
        Message::PairCert {
            name: "Mac mini".into(),
            cert: crlf_pem(),
            proof: sample_proof(),
        }
    }

    #[tokio::test]
    async fn a_paired_reply_travels_back_over_the_wire() {
        let port = host_expecting(
            |m| matches!(m, Message::PairCert { .. }),
            r#"{"type":"paired","server_cert":"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n","uniqueid":"HOST-ID","ready":false,"proof":{"account":1,"machine":"abab","signature":"cdcd"}}"#,
        )
        .await;
        assert_eq!(
            ask_cert_at("127.0.0.1", port, pair_cert_request()).await,
            CertOutcome::Paired {
                server_cert: "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
                    .into(),
                uniqueid: "HOST-ID".into(),
                ready: false,
                proof: crate::account::Proof {
                    account: 1,
                    machine: "abab".into(),
                    signature: "cdcd".into(),
                },
            }
        );
    }

    #[tokio::test]
    async fn a_refusal_is_declined_and_anything_else_is_no_peer() {
        let port = host_expecting(
            |m| matches!(m, Message::PairCert { .. }),
            r#"{"type":"declined","reason":"Dusk did not understand that request."}"#,
        )
        .await;
        assert_eq!(
            ask_cert_at("127.0.0.1", port, pair_cert_request()).await,
            CertOutcome::Declined("Dusk did not understand that request.".into())
        );

        let port = host_expecting(|m| matches!(m, Message::PairCert { .. }), "garbage").await;
        assert_eq!(
            ask_cert_at("127.0.0.1", port, pair_cert_request()).await,
            CertOutcome::NoPeer
        );
    }

    #[tokio::test]
    async fn a_host_that_hangs_up_on_a_long_line_is_no_peer() {
        // An older Dusk reads only so much of a request and closes, which
        // may reset the connection before any answer.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut first = [0u8; 64];
            let _ = stream.read(&mut first).await;
            drop(stream);
        });
        assert_eq!(
            ask_cert_at("127.0.0.1", port, pair_cert_request()).await,
            CertOutcome::NoPeer
        );

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            ask_cert_at("127.0.0.1", port, pair_cert_request()).await,
            CertOutcome::NoPeer
        );
    }

    #[test]
    fn the_client_waits_out_a_host_that_restarts_before_answering() {
        // The host may restart itself before it replies, which takes up to
        // about forty seconds.
        assert!(CERT_REPLY_TIMEOUT >= Duration::from_secs(45));
    }

    #[test]
    fn the_client_outwaits_everything_the_host_may_do_with_a_pin_request() {
        // A reply that times out is taken as the host having gone, which
        // stops the client. So it must not time out while the host is still
        // within its own limits: the person's answer and then the PIN
        // window, or the GitHub check and then the PIN window.
        assert!(REPLY_TIMEOUT > ANSWER_TIMEOUT + PIN_WINDOW);
        assert!(REPLY_TIMEOUT > crate::account::github::TIMEOUT + PIN_WINDOW);
    }
}

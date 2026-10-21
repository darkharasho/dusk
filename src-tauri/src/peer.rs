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

/// A request line is a few dozen bytes; anything long is not one.
const MAX_LINE: usize = 1024;

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
    },
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
            match pair_for_account(&state, &pin, &name, proof.as_ref()).await {
                Some(reply) => reply,
                // A host that is not set up yet gets past this, so that
                // `handle_pair` can say that instead, which is the real reason.
                None if !state.store.lock().await.allow_other_devices
                    && state.credentials.load().await.is_some() =>
                {
                    Message::Declined {
                        reason: not_on_this_account(proof.is_some()),
                    }
                }
                None => handle_pair(&app, &state, &mut write, &address, &pin, &name, gone).await,
            }
        }
        Ok(Message::Display { screen }) => match display_answer(&app, &state, screen).await {
            Some((width, height)) => Message::DisplaySize { width, height },
            None => Message::Declined {
                reason: "This machine could not tell its screen size.".into(),
            },
        },
        _ => Message::Declined {
            reason: "Dusk did not understand that request.".into(),
        },
    };
    let _ = send(&mut write, &reply).await;
}

/// The size a stream from here should be, for a machine whose screen is
/// `screen`.
///
/// Streaming the virtual display, it is the closest size the display has to
/// the asker's screen: a size it does not have would make the host give up
/// on the virtual display and stream the monitor. An asker too old to say
/// its size is treated as this machine's size.
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
    let modes = crate::host::virtual_screen::current_modes();
    crate::host::virtual_screen::best_mode(want, &modes).or(own)
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

/// Pair without asking, when the request is signed by another machine on
/// this machine's own account: the question the prompt would ask was
/// answered when both were signed in. `None` means ask as usual.
async fn pair_for_account(
    state: &Arc<AppState>,
    pin: &str,
    name: &str,
    proof: Option<&crate::account::Proof>,
) -> Option<Message> {
    let proof = proof?;
    let credentials = state.credentials.load().await?;
    if !state.account.vouches(&state.http, proof, pin).await {
        return None;
    }
    eprintln!("dusk: {name} is on this account; pairing without asking");
    Some(match enter_pin(&credentials, pin, name).await {
        Ok(()) => Message::Accepted,
        Err(err) => Message::Declined {
            reason: err.to_string(),
        },
    })
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

    match enter_pin(&credentials, pin, name).await {
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
async fn enter_pin(
    credentials: &sunshine::Credentials,
    pin: &str,
    name: &str,
) -> Result<(), sunshine::ApiError> {
    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT)?;
    let deadline = tokio::time::Instant::now() + PIN_WINDOW;
    let mut last = None;
    while tokio::time::Instant::now() < deadline {
        match api.submit_pin(credentials, pin, name).await {
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
        let next =
            tokio::time::timeout(ANSWER_TIMEOUT + Duration::from_secs(30), lines.next_line());
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

/// Ask the Dusk at `address` what size to stream at, telling it the size of
/// the screen here. `None` when there is no Dusk there, or one too old to
/// say. Kept short: a stream is waiting.
pub async fn display_size(address: &str, screen: Option<(u32, u32)>) -> Option<(u32, u32)> {
    display_size_at(address, PORT, screen).await
}

async fn display_size_at(
    address: &str,
    port: u16,
    screen: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
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
        Message::DisplaySize { width, height }
            if (320..=15360).contains(&width) && (240..=8640).contains(&height) =>
        {
            Some((width, height))
        }
        _ => None,
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
                height: 1440
            })
            .unwrap(),
            r#"{"type":"display_size","width":2560,"height":1440}"#
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

    /// A stand-in host that answers one request with `reply`.
    async fn host_answering(reply: &'static str) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            assert!(matches!(
                serde_json::from_str::<Message>(&line).unwrap(),
                Message::Display { .. }
            ));
            write.write_all(reply.as_bytes()).await.unwrap();
            write.write_all(b"\n").await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn a_host_says_how_big_its_screen_is() {
        let port = host_answering(r#"{"type":"display_size","width":3440,"height":1440}"#).await;
        assert_eq!(
            display_size_at("127.0.0.1", port, Some((3440, 1440))).await,
            Some((3440, 1440))
        );
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
}

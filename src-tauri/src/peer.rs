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
//! Nothing on the network may pair itself to a machine unattended, so the
//! host always asks: one Allow press, showing which machine is asking. That
//! is the whole difference from typing a PIN, and it is enough — the press
//! is the consent the PIN used to stand for.
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
//! connecting → host   {"type":"pair","pin":"1234","name":"Mac mini"}
//! host → connecting   {"type":"waiting"}            someone is being asked
//!                     {"type":"accepted"}           the PIN is in Sunshine
//!                     {"type":"declined","reason":"..."}
//! ```
//!
//! A connection refused means no Dusk is listening there, and the
//! connecting side falls back to showing the PIN.

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
    Pair { pin: String, name: String },
    Waiting,
    Accepted,
    Declined { reason: String },
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
        Ok(Message::Pair { pin, name }) if valid_pin(&pin) => {
            let name = clean_name(&name).unwrap_or_else(|| address.clone());
            // A dropped connection means the asking side gave up; the
            // prompt should go with it.
            let gone = async { while let Ok(Some(_)) = lines.next_line().await {} };
            handle_pair(&app, &state, &mut write, &address, &pin, &name, gone).await
        }
        _ => Message::Declined {
            reason: "Dusk did not understand that request.".into(),
        },
    };
    let _ = send(&mut write, &reply).await;
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

/// Enter the PIN into Sunshine, giving the client's own pairing request a
/// moment to arrive: Sunshine refuses a PIN nobody is waiting on, and the
/// two travel separately.
async fn enter_pin(
    credentials: &sunshine::Credentials,
    pin: &str,
    name: &str,
) -> Result<(), sunshine::ApiError> {
    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT)?;
    let mut last = None;
    for _ in 0..5 {
        match api.submit_pin(credentials, pin, name).await {
            Ok(()) => return Ok(()),
            Err(sunshine::ApiError::Unauthorized) => return Err(sunshine::ApiError::Unauthorized),
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(last.unwrap_or(sunshine::ApiError::Failed(
        "Sunshine did not take the PIN.".into(),
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
pub async fn ask(address: &str, pin: &str, on_waiting: impl FnOnce()) -> Outcome {
    ask_at(address, PORT, pin, on_waiting).await
}

async fn ask_at(address: &str, port: u16, pin: &str, on_waiting: impl FnOnce()) -> Outcome {
    let connect = TcpStream::connect((address, port));
    let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), connect).await else {
        return Outcome::NoPeer;
    };
    let (read, mut write) = stream.into_split();
    let request = Message::Pair {
        pin: pin.to_string(),
        name: host_name(),
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

fn host_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim_end_matches(".local")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_format_is_the_one_documented() {
        let pair = Message::Pair {
            pin: "0042".into(),
            name: "Mac mini".into(),
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

    #[tokio::test]
    async fn nobody_listening_is_no_peer() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let outcome = ask_at("127.0.0.1", port, "1234", || panic!("nobody to wait on")).await;
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
        let outcome = ask_at("127.0.0.1", port, "1234", || {}).await;
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
        let outcome = ask_at("127.0.0.1", port, "1234", || waited = true).await;
        assert_eq!(outcome, Outcome::Accepted);
        assert!(waited);
    }
}

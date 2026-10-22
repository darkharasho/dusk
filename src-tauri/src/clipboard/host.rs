//! B's side of clipboard sharing: the machine being streamed from.
//!
//! A TCP listener on its own port. Every connection starts with one plain
//! line (`Hello`, or `Declined` when this machine cannot do TLS at all), then
//! pinned mutual TLS, then either one `Declined` line or `Hello` and the
//! relay. Clipboard text only ever travels inside TLS, and is never logged:
//! not its contents, not its length.

use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use tokio_rustls::TlsAcceptor;

use super::local::{support, LocalClipboard, Support, System, POLL};
use super::tls::server_config;
use super::{fits, line, text_of, Decline, Guard, Wire, MAX_LINE, PORT};
use crate::model::{Activity, DEFAULT_HTTP_PORT};
use crate::state::AppState;
use crate::sunshine::state_file;
use crate::sunshine::{api, SunshineApi};

/// How often a live connection checks that a stream is still running.
const RECHECK: Duration = Duration::from_secs(5);
/// How long a peer gets to finish the TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
const VERSION: u32 = 1;

/// Pure admission decision, checked after the TLS handshake.
pub fn admit(sharing_on: bool, support: &Support, hosting: bool) -> Result<(), Decline> {
    if !sharing_on {
        return Err(Decline::SharingOff);
    }
    if let Support::Unsupported(detail) = support {
        return Err(Decline::Unsupported {
            detail: Some((*detail).to_string()),
        });
    }
    if !hosting {
        return Err(Decline::NotStreaming);
    }
    Ok(())
}

async fn send<W: AsyncWrite + Unpin>(out: &mut W, message: &Wire) -> io::Result<()> {
    out.write_all(line(message).as_bytes()).await?;
    out.flush().await
}

enum Incoming {
    Message(Wire),
    End,
}

/// Read lines until the peer ends, one capped line at a time. A line over
/// the cap, or one that is not a message, ends the connection.
async fn read_lines<R: AsyncRead + Unpin>(read: R, to: mpsc::Sender<io::Result<Incoming>>) {
    let mut reader = BufReader::new(read);
    loop {
        let mut buf = Vec::new();
        let n = (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
            .await;
        let event = match n {
            Err(err) => Err(err),
            Ok(0) => Ok(Incoming::End),
            Ok(_) if buf.last() != Some(&b'\n') && buf.len() > MAX_LINE => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a clipboard line was over the size cap",
            )),
            Ok(_) => match serde_json::from_slice::<Wire>(&buf) {
                Ok(message) => Ok(Incoming::Message(message)),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a clipboard line was not a message",
                )),
            },
        };
        let stop = !matches!(event, Ok(Incoming::Message(_)));
        if to.send(event).await.is_err() || stop {
            return;
        }
    }
}

/// `read_text` is `None` for an empty clipboard and for a read that failed;
/// neither is a change.
async fn read_clipboard<C: LocalClipboard + 'static>(clipboard: &Arc<Mutex<C>>) -> Option<String> {
    let clipboard = clipboard.clone();
    tokio::task::spawn_blocking(move || clipboard.lock().ok()?.read_text())
        .await
        .ok()
        .flatten()
}

async fn write_clipboard<C: LocalClipboard + 'static>(clipboard: &Arc<Mutex<C>>, text: String) {
    let clipboard = clipboard.clone();
    // A failed write is dropped: there is nobody to tell, and the text must
    // not end up in an error message.
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(mut clipboard) = clipboard.lock() {
            let _ = clipboard.write_text(&text);
        }
    })
    .await;
}

/// One admitted connection: Hello, then relay until either side ends or
/// `still_hosting` turns false.
pub async fn run<S, C, F, Fut>(stream: S, clipboard: C, mut still_hosting: F) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: LocalClipboard + 'static,
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let clipboard = Arc::new(Mutex::new(clipboard));
    let baseline = read_clipboard(&clipboard).await;
    let mut guard = Guard::new(baseline.as_deref());

    let (read, mut write) = tokio::io::split(stream);
    send(&mut write, &Wire::Hello { version: VERSION }).await?;

    let (tx, mut rx) = mpsc::channel(4);
    let reader = tokio::spawn(read_lines(read, tx));
    let result = relay(
        &clipboard,
        &mut guard,
        &mut write,
        &mut rx,
        &mut still_hosting,
    )
    .await;
    reader.abort();
    let _ = write.shutdown().await;
    result
}

async fn relay<C, W, F, Fut>(
    clipboard: &Arc<Mutex<C>>,
    guard: &mut Guard,
    write: &mut W,
    rx: &mut mpsc::Receiver<io::Result<Incoming>>,
    still_hosting: &mut F,
) -> io::Result<()>
where
    C: LocalClipboard + 'static,
    W: AsyncWrite + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let mut poll = interval_at(Instant::now() + POLL, POLL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recheck = interval_at(Instant::now() + RECHECK, RECHECK);
    recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            incoming = rx.recv() => match incoming {
                None | Some(Ok(Incoming::End)) | Some(Ok(Incoming::Message(Wire::Bye))) => {
                    return Ok(());
                }
                Some(Err(err)) => return Err(err),
                Some(Ok(Incoming::Message(message))) => {
                    // Hello, Declined, Unknown and non-text clips are ignored.
                    if let Some(text) = text_of(&message) {
                        if fits(text) && guard.incoming(text) {
                            write_clipboard(clipboard, text.to_string()).await;
                        }
                    }
                }
            },
            _ = poll.tick() => {
                if let Some(text) = read_clipboard(clipboard).await {
                    if fits(&text) && guard.outgoing(&text) {
                        send(write, &Wire::Clip { format: "text".into(), text }).await?;
                    }
                }
            }
            _ = recheck.tick() => {
                if !still_hosting().await {
                    send(write, &Wire::Bye).await?;
                    return Ok(());
                }
            }
        }
    }
}

// ------------------------------------------------------------------ server

/// Listen for other Dusks' clipboards. A port already taken means no
/// clipboard sharing from this machine, and nothing else.
pub fn spawn(_app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("dusk: not listening for clipboard sharing on {PORT}: {err}");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = serve(&state, stream).await {
                    // The kind only: never anything that came off the wire.
                    eprintln!("dusk: a clipboard connection ended: {:?}", err.kind());
                }
            });
        }
    });
}

/// Everything TLS needs, read fresh so a pairing made after Dusk started is
/// accepted. `None` when any of it cannot be read.
async fn tls_acceptor(state: &AppState) -> Option<TlsAcceptor> {
    let credentials = state.credentials.load().await?;
    let config = SunshineApi::new(api::DEFAULT_PORT)
        .ok()?
        .get_config(&credentials)
        .await
        .ok()?;
    let config = Some(&config);
    let cert = std::fs::read_to_string(state_file::server_cert_path(config)?).ok()?;
    let key = std::fs::read_to_string(state_file::server_key_path(config)?).ok()?;
    let paired =
        state_file::paired_certs(&state_file::read(&state_file::state_path(config)?).ok()?);
    server_config(&cert, &key, &paired)
        .ok()
        .map(TlsAcceptor::from)
}

/// B's own host is streaming right now.
async fn hosting(state: &AppState) -> bool {
    crate::serverinfo::query_http(&state.http, "127.0.0.1", DEFAULT_HTTP_PORT)
        .await
        .is_ok_and(|info| matches!(info.activity(), Activity::Hosting { .. }))
}

async fn serve(state: &Arc<AppState>, mut stream: TcpStream) -> io::Result<()> {
    // The one plain line. It never carries clipboard data.
    let Some(acceptor) = tls_acceptor(state).await else {
        let declined = Wire::Declined {
            reason: Decline::Unsupported { detail: None },
        };
        return send(&mut stream, &declined).await;
    };
    send(&mut stream, &Wire::Hello { version: VERSION }).await?;

    let mut tls = tokio::time::timeout(HANDSHAKE, acceptor.accept(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the TLS handshake timed out"))??;

    let sharing_on = state.store.lock().await.clipboard_sharing.is_on();
    let decision = admit(sharing_on, &support(), hosting(state).await);
    let clipboard = match decision {
        Ok(()) => System::open().map_err(|_| Decline::Unsupported { detail: None }),
        Err(decline) => Err(decline),
    };
    let clipboard = match clipboard {
        Ok(clipboard) => clipboard,
        Err(reason) => {
            send(&mut tls, &Wire::Declined { reason }).await?;
            return tls.shutdown().await;
        }
    };

    let for_check = state.clone();
    run(tls, clipboard, move || {
        let state = for_check.clone();
        async move { hosting(&state).await }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MAX_TEXT_BYTES;
    use tokio::io::{DuplexStream, Lines, ReadHalf, WriteHalf};

    #[derive(Clone)]
    struct Fake {
        text: Arc<Mutex<Option<String>>>,
        writes: Arc<Mutex<usize>>,
    }

    impl Fake {
        fn new(text: Option<&str>) -> Self {
            Self {
                text: Arc::new(Mutex::new(text.map(str::to_string))),
                writes: Arc::new(Mutex::new(0)),
            }
        }
        fn set(&self, text: &str) {
            *self.text.lock().unwrap() = Some(text.to_string());
        }
        fn get(&self) -> Option<String> {
            self.text.lock().unwrap().clone()
        }
        fn writes(&self) -> usize {
            *self.writes.lock().unwrap()
        }
    }

    impl LocalClipboard for Fake {
        fn read_text(&mut self) -> Option<String> {
            self.get()
        }
        fn write_text(&mut self, text: &str) -> Result<(), String> {
            *self.text.lock().unwrap() = Some(text.to_string());
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    struct Peer {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        write: WriteHalf<DuplexStream>,
    }

    impl Peer {
        async fn next(&mut self) -> Option<Wire> {
            let l = self.lines.next_line().await.unwrap()?;
            Some(serde_json::from_str(&l).unwrap())
        }
        async fn say(&mut self, message: Wire) {
            send(&mut self.write, &message).await.unwrap();
        }
        /// Whether a message arrives within `d`.
        async fn hears_within(&mut self, d: Duration) -> Option<Wire> {
            tokio::time::timeout(d, self.next()).await.ok().flatten()
        }
    }

    fn start(
        fake: &Fake,
        hosting: Arc<Mutex<bool>>,
    ) -> (Peer, tokio::task::JoinHandle<io::Result<()>>) {
        let (a, b) = tokio::io::duplex(8 * 1024 * 1024);
        let (read, write) = tokio::io::split(a);
        let fake = fake.clone();
        let task = tokio::spawn(run(b, fake, move || {
            let on = *hosting.lock().unwrap();
            async move { on }
        }));
        let peer = Peer {
            lines: BufReader::new(read).lines(),
            write,
        };
        (peer, task)
    }

    fn clip(text: &str) -> Wire {
        Wire::Clip {
            format: "text".into(),
            text: text.into(),
        }
    }

    const SEC: Duration = Duration::from_secs(1);

    #[test]
    fn admission_needs_everything() {
        assert_eq!(admit(true, &Support::Supported, true), Ok(()));
        assert_eq!(
            admit(false, &Support::Supported, true),
            Err(Decline::SharingOff)
        );
        assert_eq!(
            admit(true, &Support::Unsupported("x"), true),
            Err(Decline::Unsupported {
                detail: Some("x".into())
            })
        );
        assert_eq!(
            admit(true, &Support::Supported, false),
            Err(Decline::NotStreaming)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn host_sends_a_new_copy_but_not_the_baseline() {
        let fake = Fake::new(Some("old"));
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(true)));
        assert_eq!(peer.next().await, Some(Wire::Hello { version: 1 }));
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        fake.set("new");
        assert_eq!(peer.hears_within(SEC).await, Some(clip("new")));
    }

    #[tokio::test(start_paused = true)]
    async fn host_writes_what_arrives() {
        let fake = Fake::new(None);
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        peer.say(Wire::Hello { version: 1 }).await;
        peer.say(clip("x")).await;
        tokio::time::sleep(SEC).await;
        assert_eq!(fake.get().as_deref(), Some("x"));
        assert_eq!(fake.writes(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn host_loop_does_not_echo() {
        let fake = Fake::new(None);
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        peer.say(clip("x")).await;
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        assert_eq!(fake.writes(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn host_closes_when_streaming_stops() {
        let fake = Fake::new(None);
        let hosting = Arc::new(Mutex::new(true));
        let (mut peer, task) = start(&fake, hosting.clone());
        peer.next().await;
        *hosting.lock().unwrap() = false;
        assert_eq!(peer.hears_within(SEC * 6).await, Some(Wire::Bye));
        assert_eq!(peer.next().await, None);
        assert!(task.await.unwrap().is_ok());
        fake.set("late");
        assert_eq!(peer.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn host_skips_an_oversized_copy() {
        let fake = Fake::new(None);
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        fake.set(&"a".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(peer.hears_within(SEC * 2).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_is_not_a_change() {
        let fake = Fake::new(Some("old"));
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        *fake.text.lock().unwrap() = None;
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        fake.set("old");
        assert_eq!(peer.hears_within(SEC * 2).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn bye_from_the_peer_ends_the_connection() {
        let fake = Fake::new(None);
        let (mut peer, task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        peer.say(Wire::Bye).await;
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_over_the_cap_closes_the_connection() {
        let fake = Fake::new(None);
        let (mut peer, task) = start(&fake, Arc::new(Mutex::new(true)));
        peer.next().await;
        let junk = vec![b'a'; MAX_LINE + 10];
        peer.write.write_all(&junk).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }
}

//! B's side of clipboard sharing: the machine being streamed from.
//!
//! A TCP listener on its own port. Every connection starts with one plain
//! line (`Hello`, or `Declined` when this machine cannot do TLS at all), then
//! pinned mutual TLS, then either one `Declined` line or `Hello` and the
//! relay. Clipboard text only ever travels inside TLS, and is never logged:
//! not its contents, not its length.
//!
//! One connection is admitted at a time, and a stream's slot belongs to the
//! first client admitted during it: another paired machine is turned away
//! until the stream changes, even while the first is reconnecting. Nothing a
//! connection does before admission reads the keystore, and all of it is
//! bounded by one timeout.

use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use sha2::{Digest, Sha256};

use super::local::{support, LocalClipboard, Support, System, Watched, POLL};
use super::tls::server_config;
use super::{fits, line, text_of, Decline, Guard, Wire, MAX_LINE, PORT};
use crate::cert_pairing::reconcile::client_connected;
use crate::model::{Activity, DEFAULT_HTTP_PORT};
use crate::state::AppState;
use crate::sunshine::state_file;
use crate::sunshine::{api, SunshineApi};

/// How often a live connection checks that a stream is still running.
const RECHECK: Duration = Duration::from_secs(5);
/// How long a connection may take from accept to admission: building the
/// TLS setup, the plain line, the handshake and the checks, all together.
const ADMISSION: Duration = Duration::from_secs(10);
/// How long a declined peer is read from before the socket is dropped, so
/// the decline is not lost to a reset over bytes left unread.
const DRAIN: Duration = Duration::from_secs(1);
/// How long any one write (or the closing shutdown) may take before the
/// connection is given up on: a peer that stops reading must not pin it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one read or write of this machine's clipboard may take. A
/// clipboard owner that never answers must not pin the connection. Longer
/// than arboard's own 4 s X11 read timeout, so a slow owner (or a large
/// transfer) that arboard would finish or fail cleanly does not end the
/// connection first.
const CLIPBOARD_CALL: Duration = Duration::from_secs(5);
/// Connections that may be between accept and admission at once.
const PRE_ADMIT: usize = 8;
const VERSION: u32 = 1;

/// What a hosting check found out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosting {
    Yes,
    No,
    /// The check itself failed, which says nothing about whether it holds.
    Unknown,
}

/// Whether a busy host still has a client watching. The host stays busy
/// for as long as the app a stream launched runs, which outlasts a client
/// that dropped, so only its log says. A log that cannot be read keeps it
/// hosting, as the pairing reconciler takes it.
pub fn still_connected(busy: bool, log: Option<&str>) -> Hosting {
    match (busy, log) {
        (false, _) => Hosting::No,
        (true, Some(log)) if !client_connected(log) => Hosting::No,
        (true, _) => Hosting::Yes,
    }
}

/// The one admitted connection at a time. While another holds it, a new one
/// is declined as though nothing were streaming, which for it is the case.
pub fn claim(live: &Arc<Semaphore>) -> Result<OwnedSemaphorePermit, Decline> {
    live.clone()
        .try_acquire_owned()
        .map_err(|_| Decline::NotStreaming)
}

/// SHA-256 of a client's end-entity certificate, as presented in TLS.
pub type Fingerprint = [u8; 32];

/// The stream B's clipboard slot was last bound to, by its key, and the
/// client it was bound to. Shared by every connection.
pub type Bound = Option<(String, Fingerprint)>;

/// Which stream is running, as far as the host's log says: the whole of the
/// last line that started one. Its timestamp makes it differ from one
/// stream to the next, across restarts too. `None` when no stream started.
pub fn stream_key(log: &str) -> Option<String> {
    log.lines()
        .rev()
        .find(|line| line.contains("New streaming session started"))
        .map(|line| line.trim_end().to_string())
}

/// Binds the slot to one client per stream. The host's log carries no
/// client address, so the client is told apart by its certificate instead.
/// A different client on the bound stream is declined as though nothing
/// were streaming; a new stream, or nothing bound yet, binds this client.
/// Without a key (the log could not be read) nothing is bound or checked.
pub fn bind(bound: &mut Bound, key: Option<&str>, client: &Fingerprint) -> Result<(), Decline> {
    let Some(key) = key else {
        return Ok(());
    };
    match bound {
        Some((stream, owner)) if stream == key => {
            if owner == client {
                Ok(())
            } else {
                Err(Decline::NotStreaming)
            }
        }
        _ => {
            *bound = Some((key.to_string(), *client));
            Ok(())
        }
    }
}

/// The last step of admission, once the clipboard was opened (or not): a
/// client is bound only when it is admitted, so one declined for any
/// reason, its clipboard failing to open included, never holds the stream.
pub fn after_open<C>(
    bound: &mut Bound,
    key: Option<&str>,
    client: &Fingerprint,
    opened: Result<C, ()>,
) -> Result<C, Decline> {
    let clipboard = opened.map_err(|()| Decline::Unsupported { detail: None })?;
    bind(bound, key, client)?;
    Ok(clipboard)
}

fn fingerprint(tls: &TlsStream<TcpStream>) -> Option<Fingerprint> {
    let leaf = tls.get_ref().1.peer_certificates()?.first()?;
    Some(Sha256::digest(leaf.as_ref()).into())
}

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

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "a clipboard write timed out")
}

pub(super) async fn send<W: AsyncWrite + Unpin>(out: &mut W, message: &Wire) -> io::Result<()> {
    let write = async {
        out.write_all(line(message).as_bytes()).await?;
        out.flush().await
    };
    tokio::time::timeout(WRITE_TIMEOUT, write)
        .await
        .map_err(|_| timed_out())?
}

pub(super) async fn close<W: AsyncWrite + Unpin>(out: &mut W) -> io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, out.shutdown())
        .await
        .map_err(|_| timed_out())?
}

pub(super) enum Incoming {
    Message(Wire),
    End,
}

/// Read lines until the peer ends, one capped line at a time. A line over
/// the cap, or one that is not a message, ends the connection.
pub(super) async fn read_lines<R: AsyncRead + Unpin>(
    read: R,
    to: mpsc::Sender<io::Result<Incoming>>,
) {
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

/// Runs one clipboard call off the async threads, for at most
/// [`CLIPBOARD_CALL`]. A call that takes longer is left to finish on its
/// own and the connection is ended: its `?` releases the live slot.
async fn clipboard_call<T: Send + 'static>(
    call: impl FnOnce() -> T + Send + 'static,
) -> io::Result<T> {
    match tokio::time::timeout(CLIPBOARD_CALL, tokio::task::spawn_blocking(call)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(io::Error::other("a clipboard call failed")),
        Err(_) => {
            eprintln!("dusk: this machine's clipboard did not answer in time");
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "a clipboard call timed out",
            ))
        }
    }
}

/// `None` for an empty clipboard, a read that failed, and a clipboard whose
/// change count has not moved ([`Watched::poll`]); none of them is a change.
async fn read_clipboard<C: LocalClipboard + 'static>(
    clipboard: &Arc<Mutex<Watched<C>>>,
) -> io::Result<Option<String>> {
    let clipboard = clipboard.clone();
    clipboard_call(move || clipboard.lock().ok()?.poll()).await
}

async fn write_clipboard<C: LocalClipboard + 'static>(
    clipboard: &Arc<Mutex<Watched<C>>>,
    text: String,
) -> io::Result<()> {
    let clipboard = clipboard.clone();
    // A failed write is dropped: there is nobody to tell, and the text must
    // not end up in an error message.
    clipboard_call(move || {
        if let Ok(mut clipboard) = clipboard.lock() {
            let _ = clipboard.write_text(&text);
        }
    })
    .await
}

/// One admitted connection: Hello, then relay until either side ends, or
/// `still_hosting` or `sharing_on` turns false.
pub async fn run<S, C, F, Fut, G, GFut>(
    stream: S,
    clipboard: C,
    mut still_hosting: F,
    mut sharing_on: G,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: LocalClipboard + 'static,
    F: FnMut() -> Fut,
    Fut: Future<Output = Hosting>,
    G: FnMut() -> GFut,
    GFut: Future<Output = bool>,
{
    let clipboard = Arc::new(Mutex::new(Watched::new(clipboard)));
    let baseline = read_clipboard(&clipboard).await?;
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
        &mut sharing_on,
    )
    .await;
    reader.abort();
    let _ = close(&mut write).await;
    result
}

async fn relay<C, W, F, Fut, G, GFut>(
    clipboard: &Arc<Mutex<Watched<C>>>,
    guard: &mut Guard,
    write: &mut W,
    rx: &mut mpsc::Receiver<io::Result<Incoming>>,
    still_hosting: &mut F,
    sharing_on: &mut G,
) -> io::Result<()>
where
    C: LocalClipboard + 'static,
    W: AsyncWrite + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = Hosting>,
    G: FnMut() -> GFut,
    GFut: Future<Output = bool>,
{
    let mut poll = interval_at(Instant::now() + POLL, POLL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recheck = interval_at(Instant::now() + RECHECK, RECHECK);
    recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // One failed check is forgiven; a second in a row is not.
    let mut unknown_in_a_row = 0;

    loop {
        tokio::select! {
            incoming = rx.recv() => match incoming {
                None | Some(Ok(Incoming::End)) | Some(Ok(Incoming::Message(Wire::Bye))) => {
                    return Ok(());
                }
                Some(Err(err)) => return Err(err),
                Some(Ok(Incoming::Message(message))) => {
                    // Hello, Declined, TooBig, Unknown and non-text clips are
                    // ignored.
                    if let Some(text) = text_of(&message) {
                        if fits(text) && guard.incoming(text) {
                            write_clipboard(clipboard, text.to_string()).await?;
                        }
                    }
                }
            },
            _ = poll.tick() => {
                if let Some(text) = read_clipboard(clipboard).await? {
                    if !fits(&text) {
                        if guard.too_big(&text) {
                            send(write, &Wire::TooBig).await?;
                        }
                    } else if guard.outgoing(&text) {
                        send(write, &Wire::Clip { format: "text".into(), text }).await?;
                    }
                }
            }
            _ = recheck.tick() => {
                if !sharing_on().await {
                    send(write, &Wire::Bye).await?;
                    return Ok(());
                }
                match still_hosting().await {
                    Hosting::Yes => unknown_in_a_row = 0,
                    Hosting::Unknown if unknown_in_a_row == 0 => unknown_in_a_row = 1,
                    Hosting::No | Hosting::Unknown => {
                        send(write, &Wire::Bye).await?;
                        return Ok(());
                    }
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
        let slots = Arc::new(Semaphore::new(PRE_ADMIT));
        let live = Arc::new(Semaphore::new(1));
        let bound: Arc<Mutex<Bound>> = Arc::new(Mutex::new(None));
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            // No free slot: drop the newcomer rather than queue it.
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let (state, live, bound) = (state.clone(), live.clone(), bound.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(err) = serve(&state, stream, slot, &live, &bound).await {
                    // The kind only: never anything that came off the wire.
                    eprintln!("dusk: a clipboard connection ended: {:?}", err.kind());
                }
            });
        }
    });
}

/// Everything TLS needs, read fresh so a pairing made after Dusk started is
/// accepted. `None` when any of it cannot be read, and when the host's
/// password is not already in memory: anyone can connect, so this never
/// reads the keystore (which could put a prompt on screen).
async fn tls_acceptor(state: &AppState) -> Option<TlsAcceptor> {
    let credentials = state.credentials.cached_only()?;
    let config = SunshineApi::new(api::DEFAULT_PORT)
        .ok()?
        .get_config(&credentials)
        .await
        .ok()?;
    let (cert, key, paired) = tokio::task::spawn_blocking(move || {
        let config = Some(&config);
        let cert = std::fs::read_to_string(state_file::server_cert_path(config)?).ok()?;
        let key = std::fs::read_to_string(state_file::server_key_path(config)?).ok()?;
        let state = state_file::read(&state_file::state_path(config)?).ok()?;
        Some((cert, key, state_file::paired_certs(&state)))
    })
    .await
    .ok()??;
    server_config(&cert, &key, &paired)
        .ok()
        .map(TlsAcceptor::from)
}

/// Whether B's own host is streaming to a client right now.
async fn hosting(state: &AppState) -> Hosting {
    hosting_stream(state).await.0
}

/// Whether B's own host is streaming to a client right now, and which
/// stream it is when the log says ([`stream_key`]). The log is read only
/// with a password already in memory, never from the keystore.
async fn hosting_stream(state: &AppState) -> (Hosting, Option<String>) {
    let busy =
        match crate::serverinfo::query_http(&state.http, "127.0.0.1", DEFAULT_HTTP_PORT).await {
            Ok(info) => matches!(info.activity(), Activity::Hosting { .. }),
            Err(_) => return (Hosting::Unknown, None),
        };
    if !busy {
        return (Hosting::No, None);
    }
    let log = match (
        state.credentials.cached_only(),
        SunshineApi::new(api::DEFAULT_PORT),
    ) {
        (Some(credentials), Ok(api)) => api.get_logs(&credentials).await.ok(),
        _ => None,
    };
    let key = log.as_deref().and_then(stream_key);
    (still_connected(busy, log.as_deref()), key)
}

/// Read and discard until the peer closes, for at most [`DRAIN`].
async fn drain<R: AsyncRead + Unpin>(read: &mut R) {
    let mut buf = [0u8; 1024];
    let _ = tokio::time::timeout(DRAIN, async {
        while matches!(read.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
}

/// The TLS connection, and either what an admitted one runs with or why it
/// is declined.
type Admission = (
    TlsStream<TcpStream>,
    Result<(System, OwnedSemaphorePermit), Decline>,
);

/// Everything from accept to the admission decision. `None` when the plain
/// line already declined.
async fn admission(
    state: &AppState,
    mut stream: TcpStream,
    live: &Arc<Semaphore>,
    bound: &Mutex<Bound>,
) -> io::Result<Option<Admission>> {
    // The one plain line. It never carries clipboard data.
    let Some(acceptor) = tls_acceptor(state).await else {
        let declined = Wire::Declined {
            reason: Decline::Unsupported { detail: None },
        };
        send(&mut stream, &declined).await?;
        return Ok(None);
    };
    send(&mut stream, &Wire::Hello { version: VERSION }).await?;

    let tls = acceptor.accept(stream).await?;

    let outcome = decide(state, &tls, live, bound).await;
    Ok(Some((tls, outcome)))
}

/// The admission decision for a connection through TLS. The slot is
/// claimed before the stream is looked at, so the stream key is read while
/// no other connection can bind, and it is held (dropped on every decline)
/// until the connection ends.
async fn decide(
    state: &AppState,
    tls: &TlsStream<TcpStream>,
    live: &Arc<Semaphore>,
    bound: &Mutex<Bound>,
) -> Result<(System, OwnedSemaphorePermit), Decline> {
    let sharing_on = state.store.lock().await.clipboard_sharing.is_on();
    let support = support();
    // The checks that need nothing from the host come first.
    admit(sharing_on, &support, true)?;
    let permit = claim(live)?;
    let (hosting, key) = hosting_stream(state).await;
    admit(sharing_on, &support, hosting == Hosting::Yes)?;
    // Pinned mutual TLS always has a client certificate by now.
    let client = fingerprint(tls).ok_or(Decline::NotPaired)?;
    let opened = tokio::task::spawn_blocking(System::open)
        .await
        .map_err(|_| ())
        .and_then(|opened| opened.map_err(|_| ()));
    let mut bound = bound.lock().map_err(|_| Decline::NotStreaming)?;
    let clipboard = after_open(&mut bound, key.as_deref(), &client, opened)?;
    Ok((clipboard, permit))
}

async fn serve(
    state: &Arc<AppState>,
    stream: TcpStream,
    slot: OwnedSemaphorePermit,
    live: &Arc<Semaphore>,
    bound: &Mutex<Bound>,
) -> io::Result<()> {
    let admitted = tokio::time::timeout(ADMISSION, admission(state, stream, live, bound))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "admission timed out"))??;
    let Some((mut tls, outcome)) = admitted else {
        return Ok(());
    };
    // Held for as long as the connection runs: the one live connection.
    let (clipboard, _live) = match outcome {
        Ok(admitted) => admitted,
        Err(reason) => {
            send(&mut tls, &Wire::Declined { reason }).await?;
            let _ = close(&mut tls).await;
            drain(&mut tls).await;
            return Ok(());
        }
    };

    // Admitted: this connection no longer counts against the pre-admit limit.
    drop(slot);
    let (for_hosting, for_sharing) = (state.clone(), state.clone());
    run(
        tls,
        clipboard,
        move || {
            let state = for_hosting.clone();
            async move { hosting(&state).await }
        },
        move || {
            let state = for_sharing.clone();
            async move { state.store.lock().await.clipboard_sharing.is_on() }
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::wire::MAX_TEXT_BYTES;
    use std::sync::atomic::{AtomicBool, Ordering};
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
        hosting: Arc<Mutex<Hosting>>,
    ) -> (Peer, tokio::task::JoinHandle<io::Result<()>>) {
        start_sharing(fake, hosting, Arc::new(AtomicBool::new(true)))
    }

    fn start_sharing(
        fake: &Fake,
        hosting: Arc<Mutex<Hosting>>,
        sharing: Arc<AtomicBool>,
    ) -> (Peer, tokio::task::JoinHandle<io::Result<()>>) {
        let (a, b) = tokio::io::duplex(8 * 1024 * 1024);
        let (read, write) = tokio::io::split(a);
        let fake = fake.clone();
        let task = tokio::spawn(run(
            b,
            fake,
            move || {
                let on = *hosting.lock().unwrap();
                async move { on }
            },
            move || {
                let on = sharing.load(Ordering::SeqCst);
                async move { on }
            },
        ));
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
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        assert_eq!(peer.next().await, Some(Wire::Hello { version: 1 }));
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        fake.set("new");
        assert_eq!(peer.hears_within(SEC).await, Some(clip("new")));
    }

    #[tokio::test(start_paused = true)]
    async fn host_writes_what_arrives() {
        let fake = Fake::new(None);
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
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
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        peer.say(clip("x")).await;
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        assert_eq!(fake.writes(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn host_closes_when_streaming_stops() {
        let fake = Fake::new(None);
        let hosting = Arc::new(Mutex::new(Hosting::Yes));
        let (mut peer, task) = start(&fake, hosting.clone());
        peer.next().await;
        *hosting.lock().unwrap() = Hosting::No;
        assert_eq!(peer.hears_within(SEC * 6).await, Some(Wire::Bye));
        assert_eq!(peer.next().await, None);
        assert!(task.await.unwrap().is_ok());
        fake.set("late");
        assert_eq!(peer.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn one_failed_check_is_forgiven() {
        let fake = Fake::new(None);
        let hosting = Arc::new(Mutex::new(Hosting::Unknown));
        let (mut peer, task) = start(&fake, hosting.clone());
        peer.next().await;
        assert_eq!(peer.hears_within(SEC * 6).await, None);
        *hosting.lock().unwrap() = Hosting::Yes;
        assert_eq!(peer.hears_within(SEC * 12).await, None);
        assert!(!task.is_finished());
        // The forgiven failure was forgotten: one more is forgiven again.
        *hosting.lock().unwrap() = Hosting::Unknown;
        assert_eq!(peer.hears_within(SEC * 6).await, None);
        assert!(!task.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn two_failed_checks_in_a_row_close() {
        let fake = Fake::new(None);
        let (mut peer, task) = start(&fake, Arc::new(Mutex::new(Hosting::Unknown)));
        peer.next().await;
        assert_eq!(peer.hears_within(SEC * 11).await, Some(Wire::Bye));
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stops_reading_does_not_pin_the_connection() {
        let fake = Fake::new(None);
        let (a, b) = tokio::io::duplex(64);
        let (mut read, _write) = tokio::io::split(a);
        let task = tokio::spawn(run(
            b,
            fake.clone(),
            || async { Hosting::Yes },
            || async { true },
        ));
        // Hello fits in the buffer; read just that and then stop reading.
        let mut hello = vec![0u8; line(&Wire::Hello { version: VERSION }).len()];
        read.read_exact(&mut hello).await.unwrap();
        fake.set(&"a".repeat(100_000));
        let ended = tokio::time::timeout(WRITE_TIMEOUT + SEC * 2, task).await;
        let err = ended.expect("run returned").unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    /// A clipboard whose reads (after the first `reads` of them) and writes
    /// never return until the test lets them go by dropping its sender.
    struct Stuck {
        reads: usize,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl LocalClipboard for Stuck {
        fn read_text(&mut self) -> Option<String> {
            if self.reads == 0 {
                let _ = self.release.recv();
            } else {
                self.reads -= 1;
            }
            None
        }
        fn write_text(&mut self, _text: &str) -> Result<(), String> {
            let _ = self.release.recv();
            Ok(())
        }
    }

    /// Runs against a stuck clipboard and says how the run ended, and when.
    async fn run_stuck(reads: usize, say: Option<Wire>) -> (io::Result<()>, Duration) {
        let (_hold, release) = std::sync::mpsc::channel::<()>();
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(a);
        let mut peer = Peer {
            lines: BufReader::new(read).lines(),
            write,
        };
        let started = std::time::Instant::now();
        let task = tokio::spawn(run(
            b,
            Stuck { reads, release },
            || async { Hosting::Yes },
            || async { true },
        ));
        if let Some(message) = say {
            if peer.next().await.is_some() {
                peer.say(message).await;
            }
        }
        let ended = tokio::time::timeout(SEC * 10, task)
            .await
            .expect("run ended")
            .unwrap();
        (ended, started.elapsed())
    }

    #[tokio::test]
    async fn a_stuck_baseline_read_ends_the_connection() {
        let (ended, took) = run_stuck(0, None).await;
        assert_eq!(ended.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took >= CLIPBOARD_CALL && took < CLIPBOARD_CALL + SEC * 2);
    }

    #[tokio::test]
    async fn a_stuck_poll_ends_the_connection() {
        let (ended, took) = run_stuck(1, None).await;
        assert_eq!(ended.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took < POLL + CLIPBOARD_CALL + SEC * 2);
    }

    #[tokio::test]
    async fn a_stuck_write_ends_the_connection() {
        let (ended, took) = run_stuck(usize::MAX, Some(clip("x"))).await;
        assert_eq!(ended.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(took < CLIPBOARD_CALL + SEC * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_oversized_copy_is_reported_once_and_not_sent() {
        let fake = Fake::new(None);
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        fake.set(&"a".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(peer.hears_within(SEC).await, Some(Wire::TooBig));
        // Several more polls see the same copy: no repeat.
        assert_eq!(peer.hears_within(SEC * 3).await, None);
        fake.set(&"b".repeat(MAX_TEXT_BYTES + 1));
        assert_eq!(peer.hears_within(SEC).await, Some(Wire::TooBig));
    }

    #[tokio::test(start_paused = true)]
    async fn an_oversized_baseline_is_not_reported() {
        let fake = Fake::new(Some(&"a".repeat(MAX_TEXT_BYTES + 1)));
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        assert_eq!(peer.hears_within(SEC * 3).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn host_closes_when_sharing_is_turned_off() {
        let fake = Fake::new(None);
        let sharing = Arc::new(AtomicBool::new(true));
        let (mut peer, task) =
            start_sharing(&fake, Arc::new(Mutex::new(Hosting::Yes)), sharing.clone());
        peer.next().await;
        sharing.store(false, Ordering::SeqCst);
        assert_eq!(peer.hears_within(SEC * 6).await, Some(Wire::Bye));
        assert_eq!(peer.next().await, None);
        assert!(task.await.unwrap().is_ok());
    }

    #[test]
    fn only_one_connection_is_admitted_at_a_time() {
        let live = Arc::new(Semaphore::new(1));
        let first = claim(&live).expect("the first is admitted");
        assert_eq!(claim(&live).err(), Some(Decline::NotStreaming));
        drop(first);
        assert!(claim(&live).is_ok(), "free again once the first ends");
    }

    #[test]
    fn hosting_needs_a_client_still_connected() {
        const STARTED: &str = "Info: New streaming session started";
        const LEFT: &str = "Info: CLIENT DISCONNECTED";
        let left = [STARTED, LEFT].join("\n");
        assert_eq!(still_connected(true, Some(STARTED)), Hosting::Yes);
        assert_eq!(still_connected(true, Some(&left)), Hosting::No);
        assert_eq!(
            still_connected(true, None),
            Hosting::Yes,
            "a log that cannot be read keeps it hosting"
        );
        assert_eq!(still_connected(false, Some(STARTED)), Hosting::No);
        // And so admission turns it away.
        assert_eq!(
            admit(
                true,
                &Support::Supported,
                still_connected(true, Some(&left)) == Hosting::Yes
            ),
            Err(Decline::NotStreaming)
        );
    }

    #[test]
    fn the_stream_key_is_the_last_session_start_line() {
        let log = "Info: [10:00:00] New streaming session started\n\
                   Info: CLIENT DISCONNECTED\n\
                   Info: [10:05:00] New streaming session started\n\
                   Info: something else";
        assert_eq!(
            stream_key(log).as_deref(),
            Some("Info: [10:05:00] New streaming session started")
        );
        assert_eq!(stream_key("Info: nothing here"), None);
    }

    #[test]
    fn the_first_client_on_a_stream_binds_it() {
        let mut bound = None;
        assert_eq!(bind(&mut bound, Some("s1"), &[1; 32]), Ok(()));
        assert_eq!(bound, Some(("s1".to_string(), [1; 32])));
    }

    #[test]
    fn another_client_on_a_bound_stream_is_declined_even_after_the_first_leaves() {
        let mut bound = None;
        bind(&mut bound, Some("s1"), &[1; 32]).unwrap();
        // The first connection has ended; the binding outlives it.
        assert_eq!(
            bind(&mut bound, Some("s1"), &[2; 32]),
            Err(Decline::NotStreaming)
        );
        assert_eq!(bound, Some(("s1".to_string(), [1; 32])));
    }

    #[test]
    fn the_bound_client_may_reconnect() {
        let mut bound = None;
        bind(&mut bound, Some("s1"), &[1; 32]).unwrap();
        assert_eq!(bind(&mut bound, Some("s1"), &[1; 32]), Ok(()));
    }

    #[test]
    fn a_new_stream_rebinds() {
        let mut bound = None;
        bind(&mut bound, Some("s1"), &[1; 32]).unwrap();
        assert_eq!(bind(&mut bound, Some("s2"), &[2; 32]), Ok(()));
        assert_eq!(bound, Some(("s2".to_string(), [2; 32])));
    }

    #[test]
    fn a_client_whose_clipboard_did_not_open_is_not_bound() {
        let mut bound = None;
        assert_eq!(
            after_open::<()>(&mut bound, Some("s1"), &[1; 32], Err(())),
            Err(Decline::Unsupported { detail: None })
        );
        assert_eq!(bound, None, "declined, so not bound");
        // Another client on the same stream is then admitted and binds.
        assert_eq!(after_open(&mut bound, Some("s1"), &[2; 32], Ok(7)), Ok(7));
        assert_eq!(bound, Some(("s1".to_string(), [2; 32])));
    }

    #[test]
    fn a_client_declined_for_the_binding_leaves_it_alone() {
        let mut bound = Some(("s1".to_string(), [1; 32]));
        assert_eq!(
            after_open(&mut bound, Some("s1"), &[2; 32], Ok(())),
            Err(Decline::NotStreaming)
        );
        assert_eq!(bound, Some(("s1".to_string(), [1; 32])));
    }

    #[test]
    fn without_a_key_nothing_is_bound() {
        let mut bound = None;
        assert_eq!(bind(&mut bound, None, &[1; 32]), Ok(()));
        assert_eq!(bound, None);
        bind(&mut bound, Some("s1"), &[1; 32]).unwrap();
        assert_eq!(bind(&mut bound, None, &[2; 32]), Ok(()));
        assert_eq!(bound, Some(("s1".to_string(), [1; 32])), "left as it was");
    }

    #[tokio::test(start_paused = true)]
    async fn draining_stops_at_the_peer_closing_or_after_a_second() {
        let (mut a, b) = tokio::io::duplex(64);
        drop(b);
        let started = Instant::now();
        drain(&mut a).await;
        assert!(started.elapsed() < SEC / 10, "ends at EOF");

        let (mut a, _b) = tokio::io::duplex(64);
        let started = Instant::now();
        drain(&mut a).await;
        assert!(started.elapsed() >= DRAIN, "waits out a silent peer");
        assert!(started.elapsed() < DRAIN + SEC);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_is_not_a_change() {
        let fake = Fake::new(Some("old"));
        let (mut peer, _task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        *fake.text.lock().unwrap() = None;
        assert_eq!(peer.hears_within(SEC * 2).await, None);
        fake.set("old");
        assert_eq!(peer.hears_within(SEC * 2).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn bye_from_the_peer_ends_the_connection() {
        let fake = Fake::new(None);
        let (mut peer, task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        peer.say(Wire::Bye).await;
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_over_the_cap_closes_the_connection() {
        let fake = Fake::new(None);
        let (mut peer, task) = start(&fake, Arc::new(Mutex::new(Hosting::Yes)));
        peer.next().await;
        let junk = vec![b'a'; MAX_LINE + 10];
        peer.write.write_all(&junk).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }
}

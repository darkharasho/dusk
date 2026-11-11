//! A's side of clipboard sharing: the machine being sat at.
//!
//! Connects to the host's Dusk, then relays between the stream window (over
//! the session control channel) and the host (over pinned mutual TLS). Unlike
//! the host this side keeps no baseline or echo guard: the stream window
//! does its own, so everything it reports as a copy is sent on as it is.
//!
//! The first thing the host says is one plain line, before any TLS: `Hello`,
//! or `Declined` when it cannot do TLS at all. After `Hello` comes the
//! handshake; inside it the host sends `Hello` or a `Declined` with the
//! reason. This side sends nothing until it has read that line, and sends
//! its own `Hello` only after the host's: a decline is then never followed
//! by unread bytes, which on some systems turn the host's close into a reset
//! that loses the reason. Clipboard text only ever travels inside TLS and is
//! never logged.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use super::host::{close, read_lines, send, Incoming};
use super::tls::SERVER_NAME;
use super::{fits, text_of, Decline, Wire, PORT};
use crate::session::{Control, ToClient};

/// Event the drawer listens for when the host cannot share.
pub const UNAVAILABLE_EVENT: &str = "dusk://clipboard-unavailable";
/// What the stream window shows when a copy is over the cap.
pub const TOO_BIG_NOTICE: &str = "Too large to share — copies over 1 MB stay on this machine.";

const CONNECT: Duration = Duration::from_secs(5);
const FIRST_LINE: Duration = Duration::from_secs(5);
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long the host may take to admit or decline after the handshake. Its
/// own limit for the whole of admission is 10 s.
const ADMITTED: Duration = Duration::from_secs(15);
/// How often a quiet connection checks that sharing is still on.
const SHARING_CHECK: Duration = Duration::from_secs(5);
/// The longest plain line read before TLS. It only ever holds `Hello` or
/// `Declined`.
const PLAIN_LINE: usize = 1024;
const VERSION: u32 = 1;

/// Why the connection is over.
#[derive(Debug)]
pub enum Ended {
    /// Nothing usable on the other end: no listener, a timeout, or a failed
    /// handshake. An older Dusk lands here, and that is not a problem.
    Refused,
    /// The host would not take part, and said why.
    Declined(Decline),
    /// The stream ended, or the host left.
    Closed,
    /// There was nothing left for this connection to carry: the clipboard is
    /// off here and the host has said nothing about its pointer. Also how a
    /// run ends when sharing was turned off and on again, so the next one
    /// starts afresh.
    SharingOff,
}

/// What this side still wants from a live connection.
///
/// The connection began as the clipboard's and was gated on the clipboard's
/// setting, which made that setting the switch for the connection itself.
/// The host side was generalised when the pointer arrived; this is the same
/// move on the client, and the reason is the same: a stream whose mouse stops
/// following the host because someone turned off *clipboard* sharing is a
/// bug nobody would think to look for in that setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carry {
    /// Clipboard sharing is on here, in the same era the run began in.
    Clipboard,
    /// Clipboard sharing is off here: no copied text crosses in either
    /// direction. The connection still stands if the host is using it for
    /// the pointer, and ends if it is not.
    Nothing,
    /// Sharing was turned off and on again since this run began. It ends so
    /// the next one starts afresh -- the rule that predates the pointer,
    /// kept because a connection that outlived the off would otherwise carry
    /// the clipboard again without a fresh baseline.
    Restart,
}

/// How long after the host said no for now, or went away, before trying
/// again during the same stream.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// How the last connection of a stream ended, as far as reconnecting goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastEnd {
    /// Nothing has been tried yet on this stream: connect now, whatever the
    /// clipboard setting says, because the host may be following its pointer
    /// and only it knows.
    Fresh,
    /// The last connection had nothing left to carry here: connect again as
    /// soon as sharing is on, and otherwise at the slow cadence, since the
    /// host's own following can be turned on while the stream is up.
    OwnSetting,
    /// The host turned it down for now (sharing off there, not streaming)
    /// or closed the connection: try again after [`RETRY_AFTER`].
    HostSide,
    /// The host cannot share at all, or never answered: not again during
    /// this stream.
    GiveUp,
}

impl LastEnd {
    pub fn of(ended: &Ended) -> Self {
        match ended {
            Ended::SharingOff => LastEnd::OwnSetting,
            Ended::Closed | Ended::Declined(Decline::SharingOff | Decline::NotStreaming) => {
                LastEnd::HostSide
            }
            Ended::Refused | Ended::Declined(Decline::Unsupported { .. } | Decline::NotPaired) => {
                LastEnd::GiveUp
            }
        }
    }
}

/// A copy from the stream window made while no connection was up, kept for
/// the next one: the latest only, and only while this side's sharing has
/// not been turned off since it was made. `offs` is how many times sharing
/// had been turned off here when the copy was made. Never logged.
#[derive(Default)]
pub struct Held {
    copy: Option<(String, u64)>,
}

impl Held {
    /// Keeps `text` in place of any earlier copy.
    pub fn keep(&mut self, text: String, offs: u64) {
        self.copy = Some((text, offs));
    }

    pub fn clear(&mut self) {
        self.copy = None;
    }

    /// The copy to send on a new connection, if sharing is on now and has
    /// not been turned off since the copy was made. Empty afterwards.
    pub fn take(&mut self, sharing_on: bool, offs: u64) -> Option<String> {
        let (text, made_at) = self.copy.take()?;
        (sharing_on && made_at == offs).then_some(text)
    }
}

/// Whether to connect again, `since` the last connection ended, with this
/// side's sharing setting as it is now.
///
/// Sharing being off here is no longer a veto, only a reason not to hurry:
/// whether the connection is worth having is partly the host's answer, and
/// asking is the only way to hear it.
pub fn should_reconnect(last: &LastEnd, since: Duration, sharing_on: bool) -> bool {
    match last {
        LastEnd::Fresh => true,
        LastEnd::OwnSetting => sharing_on || since >= RETRY_AFTER,
        LastEnd::HostSide => since >= RETRY_AFTER,
        LastEnd::GiveUp => false,
    }
}

/// Connect to the host's Dusk at `address` (its host part, on [`PORT`]).
pub async fn connect(
    address: &str,
    config: Arc<rustls::ClientConfig>,
) -> Result<TlsStream<TcpStream>, Ended> {
    connect_to(crate::peer::host_part(address), PORT, config).await
}

async fn connect_to(
    host: &str,
    port: u16,
    config: Arc<rustls::ClientConfig>,
) -> Result<TlsStream<TcpStream>, Ended> {
    let mut tcp = match tokio::time::timeout(CONNECT, TcpStream::connect((host, port))).await {
        Ok(Ok(tcp)) => tcp,
        _ => return Err(Ended::Refused),
    };

    match tokio::time::timeout(FIRST_LINE, plain_line(&mut tcp)).await {
        Ok(Ok(Wire::Hello { .. })) => {}
        Ok(Ok(Wire::Declined { reason })) => return Err(Ended::Declined(reason)),
        _ => return Err(Ended::Refused),
    }

    let name = ServerName::try_from(SERVER_NAME).map_err(|_| Ended::Refused)?;
    match tokio::time::timeout(HANDSHAKE, TlsConnector::from(config).connect(name, tcp)).await {
        Ok(Ok(tls)) => Ok(tls),
        Ok(Err(err)) => {
            // The kind only: nothing that came off the wire.
            eprintln!("dusk: the clipboard handshake failed: {:?}", err.kind());
            Err(Ended::Refused)
        }
        Err(_) => Err(Ended::Refused),
    }
}

/// One line read a byte at a time, so nothing after it (the TLS handshake)
/// is consumed.
async fn plain_line(tcp: &mut TcpStream) -> io::Result<Wire> {
    let mut buf = Vec::new();
    loop {
        let byte = tcp.read_u8().await?;
        if byte == b'\n' {
            break;
        }
        buf.push(byte);
        if buf.len() > PLAIN_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
    serde_json::from_slice(&buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not a message"))
}

/// Relay until the stream ends (`from_window` closes, which also ends the
/// wait for the host's answer), the host goes away, or `carry` leaves nothing
/// for the connection to do. Nothing is sent before the host's first line,
/// and this side's `Hello` only if that line is the host's; the latest copy
/// made before then follows it.
pub async fn run<S, F, Fut>(
    stream: S,
    to_window: Control,
    mut from_window: mpsc::UnboundedReceiver<String>,
    mut carry: F,
) -> Ended
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnMut() -> Fut,
    Fut: Future<Output = Carry>,
{
    let (read, mut write) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::channel(4);
    let reader = tokio::spawn(read_lines(read, tx));

    // Copies made while connecting wait for the host's answer, and only the
    // latest of them is kept: it is the one on the clipboard.
    let mut pending = None;
    while let Ok(text) = from_window.try_recv() {
        pending = Some(text);
    }
    // The stream can end while the host is still deciding, and then there
    // is nothing left to wait for.
    let deadline = Instant::now() + ADMITTED;
    let first = loop {
        tokio::select! {
            incoming = rx.recv() => break incoming,
            copied = from_window.recv() => match copied {
                Some(text) => pending = Some(text),
                None => break None,
            },
            _ = tokio::time::sleep_until(deadline) => break None,
        }
    };

    let ended = match first {
        Some(Ok(Incoming::Message(Wire::Hello { .. }))) => {
            if send(&mut write, &Wire::Hello { version: VERSION })
                .await
                .is_err()
            {
                Ended::Closed
            } else {
                relay(
                    &mut write,
                    &to_window,
                    &mut from_window,
                    pending,
                    &mut rx,
                    &mut carry,
                )
                .await
            }
        }
        Some(Ok(Incoming::Message(Wire::Declined { reason }))) => Ended::Declined(reason),
        _ => Ended::Closed,
    };

    reader.abort();
    let _ = close(&mut write).await;
    ended
}

async fn relay<W, F, Fut>(
    write: &mut W,
    to_window: &Control,
    from_window: &mut mpsc::UnboundedReceiver<String>,
    mut pending: Option<String>,
    rx: &mut mpsc::Receiver<io::Result<Incoming>>,
    carry: &mut F,
) -> Ended
where
    W: AsyncWrite + Unpin,
    F: FnMut() -> Fut,
    Fut: Future<Output = Carry>,
{
    let mut check = interval_at(Instant::now() + SHARING_CHECK, SHARING_CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Whether the host has said anything about its pointer on this
    // connection. It states the pointer immediately after admitting a
    // following client, before anything else can happen, so by the time the
    // clipboard is first checked this is already the right answer — and the
    // host sends nothing at all when it is not following, which is the only
    // signal there is that the connection is the clipboard's alone.
    let mut following = false;

    loop {
        // What the window or the host asked for, held until what this side
        // still carries is checked: no copied text crosses once the clipboard
        // has been turned off.
        enum Forward {
            ToHost(String),
            ToWindow(ToClient),
            /// The pointer, which the clipboard's setting does not gate.
            Pointer(bool),
            /// Nothing to forward: only the check.
            Check,
        }
        let from_copy = |text: String| {
            if fits(&text) {
                Forward::ToHost(text)
            } else {
                Forward::ToWindow(ToClient::Notice {
                    text: TOO_BIG_NOTICE.into(),
                })
            }
        };
        // A copy made before the host admitted this goes first.
        let forward = if let Some(text) = pending.take() {
            from_copy(text)
        } else {
            tokio::select! {
            copied = from_window.recv() => match copied {
                None => {
                    let _ = send(write, &Wire::Bye).await;
                    return Ended::Closed;
                }
                Some(text) => from_copy(text),
            },
            incoming = rx.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Incoming::End))
                | Some(Ok(Incoming::Message(Wire::Bye))) => return Ended::Closed,
                Some(Ok(Incoming::Message(Wire::Declined { reason }))) => {
                    return Ended::Declined(reason);
                }
                Some(Ok(Incoming::Message(Wire::TooBig))) => {
                    Forward::ToWindow(ToClient::Notice { text: TOO_BIG_NOTICE.into() })
                }
                // The host's focused window took or released the pointer.
                // Passed straight through to the stream window, which is
                // what actually changes the mode and says so on screen.
                // Without this the whole chain -- KWin script, classifier,
                // watch channel, wire -- ends here, and a game taking focus
                // mid-stream changes nothing. Which is how it was found.
                Some(Ok(Incoming::Message(Wire::Pointer { locked }))) => {
                    // Noted before the check below rather than after, so the
                    // very first pointer message on a connection the
                    // clipboard is not using is not the one it dies on.
                    following = true;
                    Forward::Pointer(locked)
                }
                Some(Ok(Incoming::Message(message))) => {
                    // Hello, Unknown and non-text clips are ignored.
                    match text_of(&message).filter(|t| fits(t)) {
                        Some(text) => Forward::ToWindow(ToClient::Clipboard {
                            format: "text".into(),
                            text: text.to_string(),
                        }),
                        None => continue,
                    }
                }
            },
            _ = check.tick() => Forward::Check,
            }
        };

        let carrying = carry().await;
        match carrying {
            // Off and on again: ended here so the next connection reads a
            // fresh baseline, and it is made at once because sharing is on.
            Carry::Restart => {
                let _ = send(write, &Wire::Bye).await;
                return Ended::SharingOff;
            }
            // Off, and the host is not using this for anything else.
            Carry::Nothing if !following => {
                let _ = send(write, &Wire::Bye).await;
                return Ended::SharingOff;
            }
            _ => {}
        }
        let clipboard = carrying == Carry::Clipboard;

        match forward {
            Forward::Check => continue,
            // Never gated on the clipboard: that was the whole bug.
            Forward::Pointer(locked) => to_window.send(ToClient::MouseMode { locked }),
            // The clipboard is off here, so a copy either way is dropped
            // rather than ending a connection the pointer is still using.
            // The host does not poll its own clipboard while its side is
            // off, so a clip arriving here means its setting differs from
            // this one's, not that this check is the only thing stopping it.
            Forward::ToWindow(_) | Forward::ToHost(_) if !clipboard => continue,
            Forward::ToWindow(message) => to_window.send(message),
            Forward::ToHost(text) => {
                let clip = Wire::Clip {
                    format: "text".into(),
                    text,
                };
                if send(write, &clip).await.is_err() {
                    return Ended::Closed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::line;
    use crate::clipboard::wire::MAX_TEXT_BYTES;
    use std::sync::atomic::{AtomicU8, Ordering};
    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
    };
    use tokio::net::TcpListener;

    /// The far end of the connection, playing the host.
    struct Host {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        write: WriteHalf<DuplexStream>,
    }

    impl Host {
        async fn next(&mut self) -> Option<Wire> {
            let l = self.lines.next_line().await.unwrap()?;
            Some(serde_json::from_str(&l).unwrap())
        }
        async fn say(&mut self, message: Wire) {
            self.write
                .write_all(line(&message).as_bytes())
                .await
                .unwrap();
        }
        async fn silent_for(&mut self, d: Duration) -> bool {
            tokio::time::timeout(d, self.next()).await.is_err()
        }
    }

    struct Rig {
        host: Host,
        window: mpsc::UnboundedReceiver<ToClient>,
        copies: mpsc::UnboundedSender<String>,
        task: tokio::task::JoinHandle<Ended>,
        /// What this side says it still carries, as the window's settings
        /// would: see [`Rig::clipboard_off`] and [`Rig::restarting`].
        carry: Arc<AtomicU8>,
    }

    impl Rig {
        const CLIPBOARD: u8 = 0;
        const NOTHING: u8 = 1;
        const RESTART: u8 = 2;

        /// Clipboard sharing turned off here, which is no longer the same
        /// thing as the connection being over.
        fn clipboard_off(&self) {
            self.carry.store(Self::NOTHING, Ordering::SeqCst);
        }

        /// Turned off and on again, which ends the run whatever else it is
        /// carrying, so the next one reads a fresh baseline.
        fn restarting(&self) {
            self.carry.store(Self::RESTART, Ordering::SeqCst);
        }
    }

    /// Connected, before either side has said anything.
    fn open() -> Rig {
        let (a, b) = tokio::io::duplex(8 * 1024 * 1024);
        let (control, window) = Control::pair();
        let (copies, from_window) = mpsc::unbounded_channel();
        let carry = Arc::new(AtomicU8::new(Rig::CLIPBOARD));
        let flag = carry.clone();
        let task = tokio::spawn(run(a, control, from_window, move || {
            let now = flag.load(Ordering::SeqCst);
            async move {
                match now {
                    Rig::NOTHING => Carry::Nothing,
                    Rig::RESTART => Carry::Restart,
                    _ => Carry::Clipboard,
                }
            }
        }));
        let (read, write) = tokio::io::split(b);
        let host = Host {
            lines: BufReader::new(read).lines(),
            write,
        };
        Rig {
            host,
            window,
            copies,
            task,
            carry,
        }
    }

    async fn start() -> Rig {
        let mut rig = open();
        rig.host.say(Wire::Hello { version: 1 }).await;
        assert_eq!(rig.host.next().await, Some(Wire::Hello { version: 1 }));
        rig
    }

    #[tokio::test]
    async fn nothing_is_said_before_the_host_admits() {
        let mut rig = open();
        assert!(
            rig.host.silent_for(Duration::from_millis(200)).await,
            "the host speaks first"
        );
        rig.host
            .say(Wire::Declined {
                reason: Decline::SharingOff,
            })
            .await;
        assert!(matches!(
            rig.task.await.unwrap(),
            Ended::Declined(Decline::SharingOff)
        ));
        assert_eq!(rig.host.next().await, None, "nothing at all was sent");
    }

    #[tokio::test(start_paused = true)]
    async fn the_stream_ending_before_the_host_answers_ends_the_run_at_once() {
        let Rig {
            mut host,
            copies,
            task,
            ..
        } = open();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        drop(copies);
        let ended = tokio::time::timeout(ADMITTED, task).await;
        assert!(matches!(ended.expect("ended").unwrap(), Ended::Closed));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(host.next().await, None, "nothing was sent");
    }

    #[tokio::test]
    async fn only_the_latest_copy_made_while_connecting_is_sent() {
        let mut rig = open();
        rig.copies.send("first".into()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        rig.copies.send("second".into()).unwrap();
        rig.host.say(Wire::Hello { version: 1 }).await;
        assert_eq!(rig.host.next().await, Some(Wire::Hello { version: 1 }));
        assert_eq!(
            rig.host.next().await,
            Some(Wire::Clip {
                format: "text".into(),
                text: "second".into()
            })
        );
        assert!(rig.host.silent_for(Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn a_first_line_that_is_not_hello_ends_the_run() {
        let mut rig = open();
        rig.host.say(Wire::Bye).await;
        assert!(matches!(rig.task.await.unwrap(), Ended::Closed));
        assert_eq!(rig.host.next().await, None);
    }

    #[tokio::test]
    async fn too_big_from_the_host_becomes_a_notice() {
        let mut rig = start().await;
        rig.host.say(Wire::TooBig).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::Notice {
                text: TOO_BIG_NOTICE.into()
            })
        );
    }

    #[test]
    fn how_a_connection_ended_decides_when_to_reconnect() {
        use LastEnd::*;
        let (now, soon, later) = (Duration::ZERO, Duration::from_secs(29), RETRY_AFTER);
        for (last, since, sharing_on, expected) in [
            // Nothing tried yet: at once, and sharing being off here is not
            // a veto, because only the host knows whether it is following
            // its own pointer.
            (Fresh, now, true, true),
            (Fresh, now, false, true),
            // Sharing is off here: not in a hurry, but asked again at the
            // slow cadence in case the host started following.
            (OwnSetting, now, false, false),
            (OwnSetting, soon, false, false),
            (OwnSetting, later, false, true),
            (HostSide, later, false, true),
            // This side's own setting turned off, and is on again.
            (OwnSetting, now, true, true),
            // The host said no for now, or went: not for 30 s.
            (HostSide, now, true, false),
            (HostSide, soon, true, false),
            (HostSide, later, true, true),
            // The host cannot do it at all: never on this stream.
            (GiveUp, now, true, false),
            (GiveUp, later * 100, true, false),
        ] {
            assert_eq!(
                should_reconnect(&last, since, sharing_on),
                expected,
                "{last:?} {since:?} {sharing_on}"
            );
        }
    }

    #[test]
    fn a_copy_made_while_disconnected_waits_for_the_next_connection() {
        let mut held = Held::default();
        held.keep("first".into(), 0);
        held.keep("second".into(), 0);
        assert_eq!(
            held.take(true, 0).as_deref(),
            Some("second"),
            "the latest only"
        );
        assert_eq!(held.take(true, 0), None, "sent once");
    }

    #[test]
    fn a_held_copy_is_not_sent_once_sharing_was_turned_off_here() {
        let mut held = Held::default();
        held.keep("before".into(), 0);
        assert_eq!(held.take(false, 1), None, "off now");
        held.keep("before".into(), 0);
        assert_eq!(held.take(true, 1), None, "turned off and on again since");
        held.keep("before".into(), 0);
        held.clear();
        assert_eq!(held.take(true, 0), None);
    }

    #[test]
    fn each_ending_maps_to_a_retry_rule() {
        assert_eq!(LastEnd::of(&Ended::SharingOff), LastEnd::OwnSetting);
        assert_eq!(LastEnd::of(&Ended::Closed), LastEnd::HostSide);
        assert_eq!(
            LastEnd::of(&Ended::Declined(Decline::SharingOff)),
            LastEnd::HostSide
        );
        assert_eq!(
            LastEnd::of(&Ended::Declined(Decline::NotStreaming)),
            LastEnd::HostSide
        );
        assert_eq!(
            LastEnd::of(&Ended::Declined(Decline::Unsupported { detail: None })),
            LastEnd::GiveUp
        );
        assert_eq!(
            LastEnd::of(&Ended::Declined(Decline::NotPaired)),
            LastEnd::GiveUp
        );
        assert_eq!(LastEnd::of(&Ended::Refused), LastEnd::GiveUp);
    }

    #[tokio::test]
    async fn turning_sharing_off_ends_the_run_without_forwarding() {
        let mut rig = start().await;
        rig.clipboard_off();
        rig.host
            .say(Wire::Clip {
                format: "text".into(),
                text: "from the host".into(),
            })
            .await;
        assert!(matches!(rig.task.await.unwrap(), Ended::SharingOff));
        assert_eq!(rig.host.next().await, Some(Wire::Bye));
        assert_eq!(rig.host.next().await, None);
        assert_eq!(rig.window.recv().await, None, "nothing reached the window");
    }

    #[tokio::test]
    async fn a_copy_made_after_sharing_is_off_is_not_sent() {
        let mut rig = start().await;
        rig.clipboard_off();
        rig.copies.send("secret".into()).unwrap();
        assert!(matches!(rig.task.await.unwrap(), Ended::SharingOff));
        assert_eq!(rig.host.next().await, Some(Wire::Bye));
        assert_eq!(rig.host.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn sharing_turned_off_ends_a_quiet_run_within_a_tick() {
        let mut rig = start().await;
        rig.clipboard_off();
        let ended = tokio::time::timeout(Duration::from_secs(6), rig.task).await;
        assert!(matches!(
            ended.expect("ended on the tick").unwrap(),
            Ended::SharingOff
        ));
        assert_eq!(rig.host.next().await, Some(Wire::Bye));
    }

    #[tokio::test(start_paused = true)]
    async fn the_hosts_pointer_keeps_a_connection_the_clipboard_has_let_go_of() {
        // The second half of the bug: the pointer rides the clipboard's
        // connection, and this side made that connection only while
        // clipboard sharing was on. So turning off text sharing silently
        // turned off the mouse following the host, in a setting that says
        // nothing about the mouse.
        let mut rig = start().await;
        rig.host.say(Wire::Pointer { locked: false }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: false })
        );

        rig.clipboard_off();
        // Well past the check that used to end it.
        tokio::time::sleep(SHARING_CHECK * 3).await;

        rig.host.say(Wire::Pointer { locked: true }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: true }),
            "the pointer still arrives with the clipboard off"
        );
        assert!(!rig.task.is_finished(), "and the connection is still up");
    }

    #[tokio::test]
    async fn the_clipboard_being_off_drops_copies_rather_than_the_connection() {
        // What "off" has to keep meaning: no copied text crosses, either
        // way. It just no longer takes the pointer down with it.
        let mut rig = start().await;
        rig.host.say(Wire::Pointer { locked: true }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: true })
        );
        rig.clipboard_off();

        rig.copies.send("secret".into()).unwrap();
        rig.host
            .say(Wire::Clip {
                format: "text".into(),
                text: "from the host".into(),
            })
            .await;

        assert!(
            rig.host.silent_for(Duration::from_millis(200)).await,
            "a copy made here does not reach the host"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rig.window.recv())
                .await
                .is_err(),
            "and the host's copy does not reach the window"
        );
        assert!(!rig.task.is_finished());
    }

    #[tokio::test]
    async fn sharing_turned_off_and_on_again_ends_the_run_even_for_the_pointer() {
        // A connection that outlived the off would carry the clipboard again
        // without a fresh baseline. So this one ends, and because sharing is
        // on the next is made at once -- `LastEnd::OwnSetting`.
        let mut rig = start().await;
        rig.host.say(Wire::Pointer { locked: true }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: true })
        );

        rig.restarting();
        rig.copies.send("after the off and on".into()).unwrap();
        assert!(matches!(rig.task.await.unwrap(), Ended::SharingOff));
        assert_eq!(rig.host.next().await, Some(Wire::Bye));
        assert_eq!(
            LastEnd::of(&Ended::SharingOff),
            LastEnd::OwnSetting,
            "and with sharing on, connected again at once"
        );
    }

    #[tokio::test]
    async fn a_copy_from_the_window_reaches_the_host() {
        let mut rig = start().await;
        rig.copies.send("hello".into()).unwrap();
        assert_eq!(
            rig.host.next().await,
            Some(Wire::Clip {
                format: "text".into(),
                text: "hello".into()
            })
        );
    }

    #[tokio::test]
    async fn a_copy_from_the_host_reaches_the_window() {
        let mut rig = start().await;
        rig.host
            .say(Wire::Clip {
                format: "text".into(),
                text: "x".into(),
            })
            .await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::Clipboard {
                format: "text".into(),
                text: "x".into()
            })
        );
    }

    #[tokio::test]
    async fn the_hosts_pointer_reaches_the_stream_window() {
        // The link that was missing: the host watched its compositor and
        // said so, and this side dropped it on the floor as an unknown
        // message, so swapping into a game mid-stream changed nothing.
        let mut rig = start().await;
        rig.host.say(Wire::Pointer { locked: true }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: true })
        );

        // And back, which is the half that hands the desktop its pointer.
        rig.host.say(Wire::Pointer { locked: false }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: false })
        );
    }

    #[tokio::test]
    async fn a_pointer_message_is_not_mistaken_for_a_copy() {
        // It carries no text, so the old catch-all ignored it rather than
        // forwarding something odd. This pins that the new arm did not
        // change what the clipboard sees.
        let mut rig = start().await;
        rig.host.say(Wire::Pointer { locked: true }).await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::MouseMode { locked: true })
        );
        assert!(
            rig.host.silent_for(Duration::from_millis(200)).await,
            "nothing is sent back to the host for a pointer change"
        );
    }

    #[tokio::test]
    async fn a_non_text_clip_from_the_host_is_ignored() {
        let mut rig = start().await;
        rig.host
            .say(Wire::Clip {
                format: "png".into(),
                text: "x".into(),
            })
            .await;
        rig.host
            .say(Wire::Clip {
                format: "text".into(),
                text: "after".into(),
            })
            .await;
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::Clipboard {
                format: "text".into(),
                text: "after".into()
            })
        );
    }

    #[tokio::test]
    async fn an_oversized_copy_becomes_a_notice() {
        let mut rig = start().await;
        rig.copies.send("a".repeat(MAX_TEXT_BYTES + 1)).unwrap();
        assert_eq!(
            rig.window.recv().await,
            Some(ToClient::Notice {
                text: TOO_BIG_NOTICE.into()
            })
        );
        assert!(
            rig.host.silent_for(Duration::from_millis(200)).await,
            "the host must not see an oversized copy"
        );
    }

    #[tokio::test]
    async fn a_decline_ends_the_run_with_its_reason() {
        let mut rig = start().await;
        rig.host
            .say(Wire::Declined {
                reason: Decline::NotStreaming,
            })
            .await;
        assert!(matches!(
            rig.task.await.unwrap(),
            Ended::Declined(Decline::NotStreaming)
        ));
    }

    #[tokio::test]
    async fn the_host_leaving_ends_the_run() {
        let mut rig = start().await;
        rig.host.say(Wire::Bye).await;
        assert!(matches!(rig.task.await.unwrap(), Ended::Closed));
    }

    #[tokio::test]
    async fn the_run_ends_when_the_stream_does() {
        let rig = start().await;
        let Rig {
            mut host,
            copies,
            task,
            ..
        } = rig;
        drop(copies);
        assert!(matches!(task.await.unwrap(), Ended::Closed));
        assert_eq!(host.next().await, Some(Wire::Bye));
    }

    fn config() -> Arc<rustls::ClientConfig> {
        let ck = rcgen::generate_simple_self_signed(vec!["dusk-test".to_string()]).unwrap();
        let identity = crate::moonlight::identity::ClientIdentity {
            cert_pem: ck.cert.pem().into_bytes(),
            key_pem: ck.key_pair.serialize_pem().into_bytes(),
        };
        crate::clipboard::tls::client_config(&identity, &ck.cert.pem()).unwrap()
    }

    #[tokio::test]
    async fn nothing_listening_is_refused_quietly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let result = connect_to("127.0.0.1", port, config()).await;
        assert!(matches!(result, Err(Ended::Refused)));
    }

    #[tokio::test]
    async fn a_plain_decline_is_reported_with_its_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let declined = Wire::Declined {
                reason: Decline::Unsupported { detail: None },
            };
            tcp.write_all(line(&declined).as_bytes()).await.unwrap();
        });
        let result = connect_to("127.0.0.1", port, config()).await;
        assert!(matches!(
            result,
            Err(Ended::Declined(Decline::Unsupported { detail: None }))
        ));
    }

    #[tokio::test]
    async fn a_decline_inside_tls_arrives_and_nothing_was_sent_first() {
        let host = rcgen::generate_simple_self_signed(vec!["dusk-test".to_string()]).unwrap();
        let me = rcgen::generate_simple_self_signed(vec!["dusk-test".to_string()]).unwrap();
        let server = crate::clipboard::tls::server_config(
            &host.cert.pem(),
            &host.key_pair.serialize_pem(),
            &[me.cert.pem()],
        )
        .unwrap();
        let identity = crate::moonlight::identity::ClientIdentity {
            cert_pem: me.cert.pem().into_bytes(),
            key_pem: me.key_pair.serialize_pem().into_bytes(),
        };
        let client = crate::clipboard::tls::client_config(&identity, &host.cert.pem()).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // The host's decline path: plain Hello, handshake, Declined, close,
        // then read until the client goes.
        let host_side = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            send(&mut tcp, &Wire::Hello { version: 1 }).await.unwrap();
            let mut tls = tokio_rustls::TlsAcceptor::from(server)
                .accept(tcp)
                .await
                .unwrap();
            let declined = Wire::Declined {
                reason: Decline::NotStreaming,
            };
            send(&mut tls, &declined).await.unwrap();
            let _ = close(&mut tls).await;
            let mut heard = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(1), tls.read_to_end(&mut heard)).await;
            heard.len()
        });

        let stream = connect_to("127.0.0.1", port, client).await.unwrap();
        let (control, _window) = Control::pair();
        let (_copies, from_window) = mpsc::unbounded_channel();
        let ended = run(stream, control, from_window, || async { Carry::Clipboard }).await;
        assert!(matches!(ended, Ended::Declined(Decline::NotStreaming)));
        assert_eq!(host_side.await.unwrap(), 0, "the client said nothing");
    }

    #[tokio::test]
    async fn a_plain_line_over_1024_bytes_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let _ = tcp.write_all(&[b'a'; PLAIN_LINE + 1]).await;
            // Held open: the cap, not the peer closing, has to end the read.
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let started = std::time::Instant::now();
        let result = connect_to("127.0.0.1", port, config()).await;
        assert!(matches!(result, Err(Ended::Refused)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_host_that_fails_the_handshake_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            tcp.write_all(line(&Wire::Hello { version: 1 }).as_bytes())
                .await
                .unwrap();
            tcp.write_all(b"this is not TLS\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let result = connect_to("127.0.0.1", port, config()).await;
        assert!(matches!(result, Err(Ended::Refused)));
    }
}

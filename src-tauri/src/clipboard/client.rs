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
//! reason, and this side sends its own `Hello`. Clipboard text only ever
//! travels inside TLS and is never logged.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use super::host::{close, read_lines, send, Incoming};
use super::tls::SERVER_NAME;
use super::{fits, text_of, Decline, Wire, MAX_LINE, PORT};
use crate::session::{Control, ToClient};

/// Event the drawer listens for when the host cannot share.
pub const UNAVAILABLE_EVENT: &str = "dusk://clipboard-unavailable";
/// What the stream window shows when a copy is over the cap.
pub const TOO_BIG_NOTICE: &str = "Too large to share — copies over 1 MB stay on this machine.";

const CONNECT: Duration = Duration::from_secs(5);
const FIRST_LINE: Duration = Duration::from_secs(5);
const HANDSHAKE: Duration = Duration::from_secs(10);
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
        if buf.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
    serde_json::from_slice(&buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not a message"))
}

/// Relay until the stream ends (`from_window` closes) or the host goes away.
pub async fn run<S>(
    stream: S,
    to_window: Control,
    mut from_window: mpsc::UnboundedReceiver<String>,
) -> Ended
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
    if send(&mut write, &Wire::Hello { version: VERSION })
        .await
        .is_err()
    {
        return Ended::Closed;
    }

    let (tx, mut rx) = mpsc::channel(4);
    let reader = tokio::spawn(read_lines(read, tx));

    let ended = loop {
        tokio::select! {
            copied = from_window.recv() => match copied {
                None => {
                    let _ = send(&mut write, &Wire::Bye).await;
                    break Ended::Closed;
                }
                Some(text) if !fits(&text) => {
                    to_window.send(ToClient::Notice { text: TOO_BIG_NOTICE.into() });
                }
                Some(text) => {
                    let clip = Wire::Clip { format: "text".into(), text };
                    if send(&mut write, &clip).await.is_err() {
                        break Ended::Closed;
                    }
                }
            },
            incoming = rx.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Incoming::End))
                | Some(Ok(Incoming::Message(Wire::Bye))) => break Ended::Closed,
                Some(Ok(Incoming::Message(Wire::Declined { reason }))) => {
                    break Ended::Declined(reason);
                }
                Some(Ok(Incoming::Message(message))) => {
                    // Hello, Unknown and non-text clips are ignored.
                    if let Some(text) = text_of(&message).filter(|t| fits(t)) {
                        to_window.send(ToClient::Clipboard {
                            format: "text".into(),
                            text: text.to_string(),
                        });
                    }
                }
            },
        }
    };

    reader.abort();
    let _ = close(&mut write).await;
    ended
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::line;
    use crate::clipboard::wire::MAX_TEXT_BYTES;
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
    }

    async fn start() -> Rig {
        let (a, b) = tokio::io::duplex(8 * 1024 * 1024);
        let (control, window) = Control::pair();
        let (copies, from_window) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(a, control, from_window));
        let (read, write) = tokio::io::split(b);
        let mut host = Host {
            lines: BufReader::new(read).lines(),
            write,
        };
        assert_eq!(host.next().await, Some(Wire::Hello { version: 1 }));
        host.say(Wire::Hello { version: 1 }).await;
        Rig {
            host,
            window,
            copies,
            task,
        }
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

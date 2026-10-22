//! The control channel between Dusk and a running stream.
//!
//! Dusk listens, the client connects. One socket per session, torn down with
//! it, so there is no long-lived endpoint on the machine and no question of
//! which session a message belongs to.
//!
//! # Why the endpoint goes through the environment
//!
//! The client is told where to connect with `DUSK_SESSION_ENDPOINT` and
//! `DUSK_SESSION_TOKEN` rather than command line arguments. moonlight-qt
//! rejects arguments it does not recognise, so adding them would break every
//! client that has not learned the protocol yet — including the one Dusk
//! ships today. Unknown environment variables are simply ignored, so this
//! side can land, be tested, and sit dormant until the fork connects.
//!
//! A client that never connects costs nothing: the listener is dropped when
//! the session ends, along with the socket.

pub mod protocol;

use std::io;
use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use protocol::{line, Conversation};
pub use protocol::{Effect, OverlayAction, Stats, ToClient};

/// A listening control channel for one stream.
pub struct Session {
    endpoint: String,
    token: String,
    outgoing: mpsc::UnboundedSender<ToClient>,
    /// Unix only: removed when the session is dropped.
    socket_path: Option<PathBuf>,
}

impl Session {
    /// What to put in the client's environment so it can find us.
    pub fn env(&self) -> [(&'static str, String); 2] {
        [
            ("DUSK_SESSION_ENDPOINT", self.endpoint.clone()),
            ("DUSK_SESSION_TOKEN", self.token.clone()),
        ]
    }

    /// Ask the client to do something. Dropped silently if nothing has
    /// connected — a message to a client that is not there is not an error,
    /// it is the ordinary case for a client that never learned the protocol.
    pub fn send(&self, message: ToClient) {
        let _ = self.outgoing.send(message);
    }

    /// A handle that can talk to the client from elsewhere in Dusk, such as
    /// a button in the window, without owning the session.
    pub fn control(&self) -> Control {
        Control(self.outgoing.clone())
    }
}

/// Somewhere to send messages to one stream's client. Held outside the
/// session, so it never keeps the socket alive: once the session drops,
/// sending through this goes nowhere, as it would to a client that left.
#[derive(Clone)]
pub struct Control(mpsc::UnboundedSender<ToClient>);

impl Control {
    pub fn send(&self, message: ToClient) {
        let _ = self.0.send(message);
    }

    /// A control nothing listens to, standing in for a client that never
    /// connected to its channel.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self(tx)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A unix socket is a file and outlives the process that bound it.
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A token with enough randomness that guessing it is not a strategy.
fn token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // Refusing to start a session because the OS would not give us
        // randomness is worse than the alternative here, but a predictable
        // token must not pass silently.
        eprintln!("dusk: no system randomness for the session token");
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where this session listens.
///
/// On unix this is a path, and it has to be a short one: `sun_path` is 104
/// bytes on macOS, and a temporary directory there is already around fifty.
/// A name longer than the remainder does not fail loudly — it is truncated,
/// and the client connects to a different path than the one it was told.
#[cfg(unix)]
fn endpoint(id: &str) -> (String, Option<PathBuf>) {
    let path = std::env::temp_dir().join(format!("dusk-{id}.sock"));
    let display = path.to_string_lossy().into_owned();
    (display, Some(path))
}

/// Named pipes are not files, so there is nothing to clean up and no length
/// problem to design around.
#[cfg(windows)]
fn endpoint(id: &str) -> (String, Option<PathBuf>) {
    (format!(r"\\.\pipe\dusk-{id}"), None)
}

/// Start listening for one stream's client.
///
/// Returns the session and a stream of everything the client says that Dusk
/// should act on. The listener accepts a single connection: there is exactly
/// one client per session, and a second connection is a bug or an intruder
/// rather than a case to support.
pub async fn start() -> io::Result<(Session, mpsc::UnboundedReceiver<Effect>)> {
    let token = token();
    let id = &token[..8.min(token.len())];
    let (endpoint, socket_path) = endpoint(id);

    // Bound before returning, so the socket exists by the time the client is
    // spawned and told about it. Accepting happens in the background.
    let listener = bind(&endpoint, socket_path.as_deref()).await?;

    let (effects_tx, effects_rx) = mpsc::unbounded_channel();
    let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();

    let expected = token.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = serve(listener, expected, effects_tx, outgoing_rx).await {
            eprintln!("dusk: the session control channel ended: {err}");
        }
    });

    Ok((
        Session {
            endpoint,
            token,
            outgoing: outgoing_tx,
            socket_path,
        },
        effects_rx,
    ))
}

// ------------------------------------------------------------------- unix

#[cfg(unix)]
type Listener = tokio::net::UnixListener;

#[cfg(unix)]
async fn bind(endpoint: &str, path: Option<&std::path::Path>) -> io::Result<Listener> {
    // A socket from a previous run that did not get to clean up would make
    // bind fail with "address already in use".
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
    tokio::net::UnixListener::bind(endpoint)
}

#[cfg(unix)]
async fn accept(listener: Listener) -> io::Result<tokio::net::UnixStream> {
    let (stream, _) = listener.accept().await?;
    Ok(stream)
}

// ---------------------------------------------------------------- windows

#[cfg(windows)]
type Listener = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(windows)]
async fn bind(endpoint: &str, _path: Option<&std::path::Path>) -> io::Result<Listener> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .create(endpoint)
}

#[cfg(windows)]
async fn accept(listener: Listener) -> io::Result<Listener> {
    listener.connect().await?;
    Ok(listener)
}

// ------------------------------------------------------------------ serve

async fn serve(
    listener: Listener,
    token: String,
    effects: mpsc::UnboundedSender<Effect>,
    mut outgoing: mpsc::UnboundedReceiver<ToClient>,
) -> io::Result<()> {
    // A client that never connects (a stock one, or a fork that failed
    // before it got here) must not leave this waiting with the listener
    // open for the life of the process, once per stream. The session's
    // end is the effects receiver going away.
    let stream = tokio::select! {
        stream = accept(listener) => stream?,
        _ = effects.closed() => return Ok(()),
    };
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();
    let mut conversation = Conversation::new(token);

    loop {
        tokio::select! {
            // Writes are only attempted once the client has proved itself,
            // so nothing is sent to a peer that has not been admitted.
            Some(message) = outgoing.recv(), if conversation.authenticated() => {
                match line(&message) {
                    Ok(text) => {
                        if writer.write_all(text.as_bytes()).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(err) => eprintln!("dusk: could not encode {message:?}: {err}"),
                }
            }

            read = lines.next_line() => {
                let Some(text) = read? else { return Ok(()) };
                match conversation.accept(&text) {
                    Effect::Ignore => {}
                    Effect::Close(why) => {
                        eprintln!("dusk: closing the session channel: {why}");
                        return Ok(());
                    }
                    effect => {
                        // The receiver going away means the session is over.
                        if effects.send(effect).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_long_enough_to_be_worth_having() {
        let t = token();
        assert_eq!(t.len(), 32, "16 bytes, hex encoded");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_sessions_do_not_share_a_token() {
        assert_ne!(token(), token());
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_path_fits_in_sun_path() {
        // 104 bytes on macOS, 108 on Linux. Over that, bind does not fail —
        // the path is truncated, and the client is told an address that is
        // not the one being listened on.
        let (display, _) = endpoint("deadbeef");
        assert!(
            display.len() < 104,
            "socket path is {} bytes: {display}",
            display.len()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_that_proves_itself_is_heard() {
        use tokio::io::AsyncWriteExt;

        let (session, mut effects) = start().await.expect("listening");
        let endpoint = session.env()[0].1.clone();
        let token = session.env()[1].1.clone();

        let mut client = tokio::net::UnixStream::connect(&endpoint)
            .await
            .expect("connect");
        client
            .write_all(format!("{{\"type\":\"ready\",\"token\":\"{token}\"}}\n").as_bytes())
            .await
            .unwrap();
        client
            .write_all(b"{\"type\":\"overlay_action\",\"action\":\"disconnect\"}\n")
            .await
            .unwrap();

        assert_eq!(effects.recv().await, Some(Effect::Authenticated));
        assert_eq!(
            effects.recv().await,
            Some(Effect::Overlay(OverlayAction::Disconnect))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_with_the_wrong_token_gets_nowhere() {
        use tokio::io::AsyncWriteExt;

        let (session, mut effects) = start().await.expect("listening");
        let endpoint = session.env()[0].1.clone();

        let mut client = tokio::net::UnixStream::connect(&endpoint)
            .await
            .expect("connect");
        client
            .write_all(b"{\"type\":\"ready\",\"token\":\"guess\"}\n")
            .await
            .unwrap();
        client
            .write_all(b"{\"type\":\"overlay_action\",\"action\":\"disconnect\"}\n")
            .await
            .unwrap();

        // The channel closes without ever yielding an effect, rather than
        // the action going through.
        assert_eq!(effects.recv().await, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_nobody_connected_to_stops_listening_when_it_ends() {
        let token = token();
        let (endpoint, path) = endpoint(&token[..8]);
        let listener = bind(&endpoint, path.as_deref()).await.expect("listening");
        let (effects_tx, effects_rx) = mpsc::unbounded_channel();
        let (_outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();
        let serving = tokio::spawn(serve(listener, token, effects_tx, outgoing_rx));

        // The stream ends without any client having connected.
        drop(effects_rx);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(2), serving).await;
        assert!(
            matches!(ended, Ok(Ok(Ok(())))),
            "the listener task must end with the session, not with the process: {ended:?}"
        );
        if let Some(path) = path {
            let _ = std::fs::remove_file(path);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_socket_is_gone_once_the_session_is() {
        let (session, _effects) = start().await.expect("listening");
        let path = PathBuf::from(session.env()[0].1.clone());
        assert!(path.exists());
        drop(session);
        assert!(!path.exists(), "a unix socket outlives its process");
    }
}

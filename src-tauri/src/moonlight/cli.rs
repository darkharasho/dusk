//! Driving moonlight-qt as a subprocess.
//!
//! Only the three actions that genuinely need it live here — pairing,
//! streaming and quitting a session. The app list comes from GameStream's
//! `applist` endpoint instead (see [`crate::applist`]): it is structured XML
//! rather than CLI text, and Dusk already holds the certificate it needs.
//!
//! Two things measured against moonlight-qt 6.x rather than assumed:
//!
//! - It exits **255** on failure, so the exit status is trustworthy.
//! - Everything human-readable, including a `Redirecting log output to ...`
//!   banner it prints on every run, goes to **stderr**. stdout carries only
//!   data, which is why errors here are built from stderr with that banner
//!   stripped.
//! - Except a stream's own failures. Those go only to the log file the
//!   banner names, so stderr holds the banner and nothing else, and the
//!   reason has to be fetched from that file. See [`reason_from_log`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;

/// Pairing waits on a person walking to another machine and typing four
/// digits, so it gets a generous budget.
pub(crate) const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

/// Everything else talks to a host that is either there or not. Measured
/// against moonlight-qt 6.x, `quit` against a host that will not answer
/// hangs indefinitely rather than failing — hence a timeout at all.
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);

/// How many trailing stderr lines to keep from a running stream. Enough to
/// carry the reason a session ended plus the context around it, without
/// holding a whole session's FFmpeg trace in memory.
const STDERR_TAIL: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum MoonlightError {
    #[error("could not start the stream: {0}")]
    Spawn(String),
    #[error("The streaming client did not answer, so Dusk stopped waiting")]
    TimedOut,
    #[error("{0}")]
    Failed(String),
    /// Another copy of the client is open and holds its settings, so nothing
    /// can be written to them until it closes.
    #[error("Close the stream window to finish pairing.")]
    ClientOpen,
}

/// Which Moonlight Dusk ended up driving.
///
/// This decides whether the in-stream overlay — the floating handle that ends
/// a session or changes quality without leaving the stream — exists at all.
/// It lives only in Dusk's fork, so a stock install streams perfectly well and
/// simply has no overlay, which is indistinguishable from a broken one unless
/// Dusk says which it got.
///
/// Three states rather than two, for the same reason the Sunshine sign-in has
/// three: `Unknown` is what an explicit `DUSK_MOONLIGHT_BIN` leaves us with,
/// and claiming either answer there would be a guess presented as a fact.
///
/// Decided by provenance — which search found the binary. The fork does log a
/// line identifying itself at startup, but into moonlight-qt's redirected log
/// file rather than to stdout, so it cannot be read without launching a
/// stream. A marker on `--version` would make this a fact instead, and
/// belongs in the fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Dusk's own fork, which carries the overlay.
    Dusk,
    /// A stock moonlight-qt found on this machine. No overlay.
    System,
    /// Pointed at by hand, so Dusk cannot tell which it is.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Moonlight {
    binary: PathBuf,
    kind: ClientKind,
    /// Whether this client can record a host it never paired with itself,
    /// once it has answered. See [`Moonlight::supports_add_host`].
    add_host: OnceCell<bool>,
}

/// Stream settings Dusk passes through. Everything omitted is left to
/// moonlight-qt's own configuration rather than second-guessed here.
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    pub resolution: Option<(u32, u32)>,
    pub fps: Option<u32>,
    /// Kbps.
    pub bitrate: Option<u32>,
    /// `borderless`, `fullscreen` or `windowed`.
    pub display_mode: Option<String>,
    /// Left out for Automatic, so a stock client too old to know a codec
    /// name is only ever handed one somebody chose.
    pub codec: Option<crate::store::Codec>,
}

/// The client's name for a codec on its command line.
fn codec_arg(codec: crate::store::Codec) -> &'static str {
    use crate::store::Codec;
    match codec {
        Codec::Auto => "auto",
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    }
}

/// The command line for a stream.
fn stream_args(host: &str, app: &str, options: &StreamOptions) -> Vec<String> {
    let mut args: Vec<String> = vec!["stream".into(), host.into(), app.into()];

    if let Some((w, h)) = options.resolution {
        args.push("--resolution".into());
        args.push(format!("{w}x{h}"));
    }
    if let Some(fps) = options.fps {
        args.push("--fps".into());
        args.push(fps.to_string());
    }
    if let Some(bitrate) = options.bitrate {
        args.push("--bitrate".into());
        args.push(bitrate.to_string());
    }
    if let Some(mode) = &options.display_mode {
        args.push("--display-mode".into());
        args.push(mode.clone());
    }
    if let Some(codec) = options.codec {
        args.push("--video-codec".into());
        args.push(codec_arg(codec).into());
    }
    args
}

impl Moonlight {
    /// A client at a known path. Only tests construct one this way —
    /// everything else arrives through [`Moonlight::discover`], which is also
    /// what establishes the [`ClientKind`].
    #[cfg(test)]
    pub fn at(binary: PathBuf) -> Self {
        Self::new(binary, ClientKind::Unknown)
    }

    fn new(binary: PathBuf, kind: ClientKind) -> Self {
        Self {
            binary,
            kind,
            add_host: OnceCell::new(),
        }
    }

    /// Whether the in-stream overlay is available, or `None` when Dusk cannot
    /// tell because the binary was pointed at by hand.
    pub fn overlay(&self) -> Option<bool> {
        match self.kind {
            ClientKind::Dusk => Some(true),
            ClientKind::System => Some(false),
            ClientKind::Unknown => None,
        }
    }

    /// The client Dusk will actually run. Which settings store is the right
    /// one to read depends on it — see [`crate::moonlight::stores_for`].
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    /// Find a Moonlight to drive, or `None` if there is none.
    ///
    /// `resources` is the bundle's resource directory, where a packaged Dusk
    /// carries its own client. It is passed in rather than derived here
    /// because only Tauri knows where that lands on each platform — it is
    /// `Contents/Resources` inside a .app, beside the executable on Windows,
    /// and under `usr/lib` in a Linux package.
    ///
    /// `DUSK_MOONLIGHT_BIN` overrides the search, which is what makes a
    /// portable or Flatpak install usable without Dusk having to enumerate
    /// every packaging scheme.
    pub fn discover(resources: Option<&Path>) -> Option<Self> {
        if let Some(path) = std::env::var_os("DUSK_MOONLIGHT_BIN") {
            let path = PathBuf::from(path);
            if path.is_file() {
                // Deliberately not claimed as either one. Someone pointing
                // Dusk at a binary may be pointing it at the fork or at a
                // packaged stock build, and the UI should not assert which.
                return Some(Self::new(path, ClientKind::Unknown));
            }
            eprintln!("dusk: DUSK_MOONLIGHT_BIN is set but {path:?} is not a file");
        }

        // Dusk's own client wins over any system Moonlight. It is a fork
        // carrying the in-stream overlay, so picking up a stock install
        // instead does not fail loudly — it streams perfectly well and the
        // overlay is simply absent, which reads as a bug in Dusk.
        if let Some(path) = dusk_client(resources).into_iter().find(|p| p.is_file()) {
            eprintln!("dusk: using Dusk's Moonlight client at {path:?}");
            return Some(Self::new(path, ClientKind::Dusk));
        }

        let found = candidates().into_iter().find(|p| p.is_file());
        if let Some(path) = &found {
            eprintln!("dusk: using system Moonlight at {path:?}; the in-stream overlay will be unavailable");
        }
        found.map(|binary| Self::new(binary, ClientKind::System))
    }

    /// Pair with a host, sending the PIN the user was shown.
    ///
    /// moonlight-qt mints the client identity here if it has none, which is
    /// why Dusk never writes to its store — pairing is what creates one.
    pub async fn pair(&self, host: &str, pin: &str) -> Result<(), MoonlightError> {
        self.run(&["pair", host, "--pin", pin], PAIR_TIMEOUT)
            .await
            .map(drop)
    }

    /// Whether this client can record a host Dusk paired it with, so pairing
    /// needs no PIN. Only Dusk's own fork can, and only a fork new enough to
    /// say so on `--version`; asked once per run.
    ///
    /// A stock client is never asked. It has no such action, and an
    /// argument it does not know starts its whole interface.
    pub async fn supports_add_host(&self) -> bool {
        if self.kind != ClientKind::Dusk {
            return false;
        }
        // Only an answer is kept. A client that could not be asked, timed
        // out or would not start, is asked again next time.
        let answer = self
            .add_host
            .get_or_try_init(|| async {
                let stdout = self.run(&["--version"], ACTION_TIMEOUT).await?;
                Ok::<_, MoonlightError>(advertises_add_host(&stdout))
            })
            .await;
        match answer {
            Ok(supported) => *supported,
            Err(err) => {
                eprintln!("dusk: could not ask the client what it supports: {err}");
                false
            }
        }
    }

    /// Record a host as paired, under the certificate it gave Dusk.
    ///
    /// `address` may carry the host's HTTP port, which is passed on as one.
    /// The certificate goes through a file of its own for the length of the
    /// call, because that is how the client takes one.
    pub async fn add_host(
        &self,
        address: &str,
        uuid: &str,
        name: &str,
        server_cert_pem: &str,
    ) -> Result<(), MoonlightError> {
        let cert = CertFile::write(server_cert_pem)
            .map_err(|e| MoonlightError::Failed(format!("could not stage the certificate: {e}")))?;
        let args = add_host_args(address, uuid, name, &cert.0);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.output(&args, ACTION_TIMEOUT).await?;
        if output.status.success() {
            return Ok(());
        }
        // 2 is the fork saying another copy of the client is open.
        if output.status.code() == Some(2) {
            return Err(MoonlightError::ClientOpen);
        }
        Err(MoonlightError::Failed(clean_stderr(
            &String::from_utf8_lossy(&output.stderr),
        )))
    }

    /// Ask the host to end whatever session is running.
    pub async fn quit(&self, host: &str) -> Result<(), MoonlightError> {
        self.run(&["quit", host], ACTION_TIMEOUT).await.map(drop)
    }

    /// Launch a stream. The child runs for the life of the session, so this
    /// hands it back rather than waiting on it.
    ///
    /// `env` carries the control channel's address and token. It goes in the
    /// environment rather than argv because moonlight-qt rejects arguments it
    /// does not recognise — so a client that has not learned the protocol
    /// ignores these, where an argument would stop it starting at all.
    pub fn stream(
        &self,
        host: &str,
        app: &str,
        options: &StreamOptions,
        env: &[(&'static str, String)],
    ) -> Result<Child, MoonlightError> {
        let args = stream_args(host, app, options);

        let mut command = Command::new(&self.binary);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(false);

        for (key, value) in env {
            command.env(key, value);
        }

        command
            .spawn()
            .map_err(|e| MoonlightError::Spawn(e.to_string()))
    }

    /// Wait for a stream to end, draining its stderr the whole time.
    ///
    /// Draining is not bookkeeping, it is the difference between a stream
    /// that explains itself and one that dies in silence. [`Self::stream`]
    /// hands back a child with stderr on a pipe; left unread, two things go
    /// wrong. moonlight-qt is extremely chatty — a single decoded frame can
    /// produce half a dozen lines — so the pipe is a slow fuse under a long
    /// session. And with nothing kept, a client that exits 255 one second in
    /// tells us nothing: the card flips back to Ready and the person is left
    /// looking at a button that apparently did nothing.
    ///
    /// Only the tail is kept. The reason a stream failed is always at the
    /// end, and a session that ran for an hour would otherwise have us
    /// holding megabytes of FFmpeg trace to quote one line of.
    ///
    /// All of it goes to `log`, though, which is on disk: a stream that
    /// freezes the machine never gets to fail, and its tail in memory is lost
    /// with everything else.
    pub async fn wait_for_session(
        child: &mut Child,
        log: Option<super::stream_log::StreamLog>,
    ) -> Result<(), MoonlightError> {
        let drain = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL);
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(log) = &log {
                        log.line(&line);
                    }
                    if tail.len() == STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                Vec::from(tail).join("\n")
            })
        });

        let status = child
            .wait()
            .await
            .map_err(|e| MoonlightError::Spawn(e.to_string()))?;

        // Awaited after the child is reaped, so the pipe is closed and this
        // cannot outlive the session it belongs to.
        let stderr = match drain {
            Some(handle) => handle.await.unwrap_or_default(),
            None => String::new(),
        };

        if status.success() {
            return Ok(());
        }
        let message = reason(&stderr)
            .or_else(|| crashed(status).then(|| "The streaming client crashed.".to_string()))
            .unwrap_or_else(|| NO_REASON.into());
        Err(MoonlightError::Failed(message))
    }

    async fn run(&self, args: &[&str], budget: Duration) -> Result<String, MoonlightError> {
        let output = self.output(args, budget).await?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        Err(MoonlightError::Failed(clean_stderr(
            &String::from_utf8_lossy(&output.stderr),
        )))
    }

    async fn output(
        &self,
        args: &[&str],
        budget: Duration,
    ) -> Result<std::process::Output, MoonlightError> {
        // kill_on_drop matters here: on timeout the future below is dropped,
        // which drops the child, and without this the process would be left
        // running — which is exactly the hang we are guarding against.
        let child = Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| MoonlightError::Spawn(e.to_string()))?;

        match tokio::time::timeout(budget, child.wait_with_output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(MoonlightError::Spawn(e.to_string())),
            Err(_) => Err(MoonlightError::TimedOut),
        }
    }
}

/// The command line that records a paired host. `add-host` comes first,
/// because the client picks its action from the first word it knows, and a
/// host named `list` must not be read as one.
///
/// Each value is joined to its option with `=`. On Linux, Qt's X11 plugin
/// takes `--name <value>` as its own option and removes both words before
/// the client sees them, which failed every pairing there with "--name not
/// provided". It leaves `--name=<value>` alone.
fn add_host_args(address: &str, uuid: &str, name: &str, cert: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "add-host".into(),
        crate::peer::host_part(address).into(),
        format!("--uuid={uuid}"),
        format!("--name={name}"),
        format!("--server-cert={}", cert.to_string_lossy()),
    ];
    if let Some(port) = port_part(address) {
        args.push(format!("--port={port}"));
    }
    args
}

/// The port an address names, if it names one: `10.0.0.5:47989` and
/// `[fe80::1]:47989` do, `10.0.0.5` and a bare `fe80::1` do not.
fn port_part(address: &str) -> Option<u16> {
    let port = match address.strip_prefix('[') {
        Some(rest) => rest.split_once("]:")?.1,
        None if address.matches(':').count() == 1 => address.split_once(':')?.1,
        None => return None,
    };
    port.parse().ok()
}

/// A server certificate written out for the client to read, removed again
/// when this is dropped.
struct CertFile(PathBuf);

impl CertFile {
    fn write(pem: &str) -> std::io::Result<Self> {
        use std::io::Write;
        let mut id = [0u8; 8];
        getrandom::getrandom(&mut id).map_err(|e| std::io::Error::other(e.to_string()))?;
        let path = std::env::temp_dir().join(format!(
            "dusk-server-cert-{}.pem",
            crate::install::download::hex(&id)
        ));
        // A fresh name, never an existing file: the temp directory is shared.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        // Owned before writing, so a failed write still removes the file.
        let cert = CertFile(path);
        file.write_all(pem.as_bytes())?;
        Ok(cert)
    }
}

impl Drop for CertFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What the fork prints on `--version`, on stdout, when it has `add-host`.
const ADD_HOST_MARKER: &str = "Dusk fork: add-host available";

fn advertises_add_host(stdout: &str) -> bool {
    stdout.lines().any(|line| line.trim() == ADD_HOST_MARKER)
}

/// The fork announces itself on stderr when it starts, ahead of anything
/// that went wrong. Those lines are never the reason.
const FORK_BANNER: &str = "Qt Info: Dusk fork:";

const NO_REASON: &str = "The stream failed but gave no reason.";

const LOG_BANNER: &str = "Redirecting log output to ";

/// How much of the end of a client log to read. The reason a stream ended
/// is in its last few lines; a long session's log is megabytes of trace.
const LOG_TAIL_BYTES: usize = 256 * 1024;

/// Turn moonlight-qt's stderr into something worth showing a person.
fn clean_stderr(stderr: &str) -> String {
    reason(stderr).unwrap_or_else(|| NO_REASON.into())
}

/// Why moonlight-qt failed, if it said.
///
/// The last real line of stderr when there is one: the earlier lines are Qt
/// plumbing, the last is the reason. Failing that, the log file the banner
/// points at, which is where a stream's failures actually go.
fn reason(stderr: &str) -> Option<String> {
    let last = stderr.lines().map(str::trim).rfind(|line| {
        !line.is_empty() && !line.starts_with(LOG_BANNER) && !line.contains(FORK_BANNER)
    });
    if let Some(line) = last {
        return Some(line.to_string());
    }

    let path = log_path(stderr)?;
    let found = read_tail(&path).as_deref().and_then(reason_from_log);
    if found.is_none() {
        eprintln!("dusk: the streaming client failed without a reason; its log is {path:?}");
    }
    found
}

/// The log file named by the banner moonlight-qt prints when it starts.
fn log_path(stderr: &str) -> Option<PathBuf> {
    stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix(LOG_BANNER))
        .map(|path| PathBuf::from(path.trim()))
        .filter(|path| !path.as_os_str().is_empty())
}

fn read_tail(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

/// Pull the reason a stream failed out of a moonlight-qt log.
///
/// Each entry starts `HH:MM:SS - `, and one can run on over several lines:
/// the client's own error dialogs are written whole, blank lines included.
/// What it put in front of the person is logged at `Qt Critical` (QML's
/// `console.error`), so the last of those is the reason. Without one, the
/// last `SDL Error` — the streaming library's own account, terser but still
/// better than nothing.
fn reason_from_log(log: &str) -> Option<String> {
    let mut entries: Vec<Vec<&str>> = Vec::new();
    for line in log.lines() {
        match line.get(8..11) {
            Some(" - ") if is_timestamp(&line[..8]) => entries.push(vec![&line[11..]]),
            _ => {
                if let Some(entry) = entries.last_mut() {
                    entry.push(line);
                }
            }
        }
    }

    let message = |entry: &Vec<&str>, prefixes: &[&str]| -> Option<String> {
        let first = prefixes.iter().find_map(|p| entry[0].strip_prefix(p))?;
        Some(sentence(
            std::iter::once(first.trim_start_matches("qml: ")).chain(entry[1..].iter().copied()),
        ))
    };
    let shown = ["Qt Critical: ", "Qt Fatal: "];
    entries
        .iter()
        .rev()
        .find_map(|e| message(e, &shown))
        .or_else(|| {
            entries.iter().rev().find_map(|e| {
                let rest = ["SDL Error (", "SDL Critical ("]
                    .iter()
                    .find_map(|p| e[0].strip_prefix(p))?;
                let (_, text) = rest.split_once("): ")?;
                Some(sentence(
                    std::iter::once(text).chain(e[1..].iter().copied()),
                ))
            })
        })
        .filter(|m| !m.is_empty())
}

fn is_timestamp(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 8
        && b[2] == b':'
        && b[5] == b':'
        && [0, 1, 3, 4, 6, 7].iter().all(|&i| b[i].is_ascii_digit())
}

/// Lines of a dialog as one sentence: "Connection terminated" over "Error
/// code: -1" reads as "Connection terminated. Error code: -1".
fn sentence<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for line in lines.map(str::trim).filter(|l| !l.is_empty()) {
        if !out.is_empty() {
            if !out.ends_with(['.', '!', '?', ':']) {
                out.push('.');
            }
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// Whether the client died rather than exited: killed by a signal on Unix,
/// an NTSTATUS exception code (0xC…) on Windows.
fn crashed(status: std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().is_some()
    }
    #[cfg(windows)]
    {
        status.code().is_some_and(|c| (c as u32) >= 0xC000_0000)
    }
}

/// The forked client as it sits in the fork's own build tree, which is what
/// a sibling checkout offers.
///
/// Development only, like the search that uses it — a release build carries
/// its own client and must never reach outside itself for one.
#[cfg(all(debug_assertions, target_os = "macos"))]
const BUILT_CLIENT: &str = "app/Moonlight.app/Contents/MacOS/Moonlight";
#[cfg(all(debug_assertions, target_os = "linux"))]
const BUILT_CLIENT: &str = "app/moonlight";
#[cfg(all(debug_assertions, target_os = "windows"))]
const BUILT_CLIENT: &str = "app\\release\\Moonlight.exe";

/// The forked client as Dusk bundles it, relative to the resource directory.
///
/// A flat `client/` rather than the fork's build layout: what Dusk ships is
/// an artifact, and reproducing qmake's directory names inside a shipped
/// bundle would tie the install layout to how the fork happens to build.
#[cfg(target_os = "linux")]
const BUNDLED_CLIENT: &str = "client/moonlight";
#[cfg(target_os = "windows")]
const BUNDLED_CLIENT: &str = "client\\Moonlight.exe";

/// The forked client inside a macOS bundle, relative to Dusk's executable.
///
/// `Contents/Helpers` rather than `Contents/Resources`, and found from the
/// executable rather than from the resource directory, because Tauri's
/// resource copier does not preserve symlinks. A Qt framework is a versioned
/// bundle whose top-level binary is a symlink into `Versions/Current`;
/// flattening that duplicates every framework binary and breaks the structure
/// the signature seals, which notarization rejects with "the signature of the
/// binary is invalid". So the client is inserted after Tauri has bundled, by
/// scripts/package-macos.sh, into the directory Apple expects nested helper
/// apps to live in.
#[cfg(target_os = "macos")]
const HELPER_CLIENT: &str = "../Helpers/Moonlight.app/Contents/MacOS/Moonlight";

/// Where Dusk's own Moonlight client might be.
///
/// Two places, and both are real.
///
/// Packaged, it is in the bundle's resource directory, because that is the
/// one mechanism that puts a file inside the app on all three platforms
/// without hand-rolling a layout per bundler.
///
/// In development it is a sibling checkout of the Dusk repository, which is
/// how the fork is cloned. Without that path a dev build silently falls
/// through to whatever Moonlight is installed system-wide and the overlay
/// appears broken when it is merely absent.
///
/// Until the resource path existed, only the development one did — which is
/// exactly why every release build so far has streamed without an overlay.
fn dusk_client(resources: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let _ = &resources;

    // macOS keeps its client beside the executable in Contents/Helpers, for
    // the symlink reason spelled out on HELPER_CLIENT.
    #[cfg(target_os = "macos")]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(HELPER_CLIENT));
        }
    }

    // Windows and Linux have no symlinks to lose — a flat directory of DLLs
    // and a single AppImage — so there the resource directory is the simpler
    // mechanism and keeps working.
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = resources {
        out.push(dir.join(BUNDLED_CLIENT));
    }

    // Development only: a release build must never reach outside itself.
    #[cfg(debug_assertions)]
    {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(workspace) = manifest.parent().and_then(|p| p.parent()) {
            out.push(workspace.join("moonlight-qt").join(BUILT_CLIENT));
        }
    }

    out
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from(
        "/Applications/Moonlight.app/Contents/MacOS/Moonlight",
    )];
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join("Applications/Moonlight.app/Contents/MacOS/Moonlight"));
    }
    out
}

#[cfg(target_os = "linux")]
fn candidates() -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from("/usr/bin/moonlight"),
        PathBuf::from("/usr/local/bin/moonlight"),
        PathBuf::from("/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        out.push(
            PathBuf::from(home)
                .join(".local/share/flatpak/exports/bin/com.moonlight_stream.Moonlight"),
        );
    }
    out.extend(on_path("moonlight"));
    out
}

#[cfg(target_os = "windows")]
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            out.push(PathBuf::from(base).join("Moonlight Game Streaming\\Moonlight.exe"));
        }
    }
    out.extend(on_path("Moonlight.exe"));
    out
}

/// Walk `PATH` for a name. Used where a package manager may have put the
/// binary somewhere the fixed list does not know about.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn on_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_bitrate_and_codec_leave_the_client_to_choose() {
        let options = StreamOptions {
            resolution: Some((1920, 1080)),
            ..Default::default()
        };
        assert_eq!(
            stream_args("host", "Desktop", &options),
            ["stream", "host", "Desktop", "--resolution", "1920x1080"]
        );
    }

    #[test]
    fn a_chosen_bitrate_and_codec_are_passed_in_the_clients_own_words() {
        let options = StreamOptions {
            bitrate: Some(40_000),
            codec: Some(crate::store::Codec::H264),
            ..Default::default()
        };
        assert_eq!(
            stream_args("host", "Desktop", &options),
            [
                "stream",
                "host",
                "Desktop",
                "--bitrate",
                "40000",
                "--video-codec",
                "H.264"
            ]
        );
        assert_eq!(codec_arg(crate::store::Codec::Hevc), "HEVC");
        assert_eq!(codec_arg(crate::store::Codec::Av1), "AV1");
    }

    /// Stand-ins for moonlight-qt, so the timeout and success paths are
    /// covered on every platform rather than only on the developer's.
    ///
    /// Windows has no `/bin/sleep`; a `ping` with a count is the usual way
    /// to occupy a shell for a known time.
    #[cfg(unix)]
    const HANGS: (&str, &[&str]) = ("/bin/sleep", &["60"]);
    #[cfg(windows)]
    const HANGS: (&str, &[&str]) = ("cmd", &["/C", "ping", "-n", "60", "127.0.0.1"]);

    #[cfg(unix)]
    const PRINTS_READY: (&str, &[&str]) = ("/bin/echo", &["ready"]);
    #[cfg(windows)]
    const PRINTS_READY: (&str, &[&str]) = ("cmd", &["/C", "echo", "ready"]);

    /// Any file that certainly exists, for the discovery override.
    fn a_real_file() -> PathBuf {
        #[cfg(unix)]
        return PathBuf::from("/bin/sh");
        #[cfg(windows)]
        return PathBuf::from(
            std::env::var("COMSPEC").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string()),
        );
    }

    #[test]
    fn the_log_banner_is_not_shown_to_a_person() {
        let stderr = "Redirecting log output to /tmp/Moonlight-123.log\n\
                      Computer Michaels-Mini has not been paired. Please open Moonlight to pair before retrieving games list.\n";
        assert_eq!(
            clean_stderr(stderr),
            "Computer Michaels-Mini has not been paired. Please open Moonlight to pair before retrieving games list."
        );
    }

    #[test]
    fn the_last_real_line_is_the_reason() {
        let stderr = "Redirecting log output to /tmp/x.log\nqt.qpa noise\nHost is unreachable\n";
        assert_eq!(clean_stderr(stderr), "Host is unreachable");
    }

    #[test]
    fn silence_still_produces_a_sentence() {
        assert_eq!(
            clean_stderr("Redirecting log output to /tmp/x.log\n"),
            "The stream failed but gave no reason."
        );
        assert_eq!(clean_stderr(""), "The stream failed but gave no reason.");
    }

    #[test]
    fn a_dialog_written_only_to_the_log_is_the_reason() {
        let log = "00:00:01 - Qt Info: Dusk fork: in-stream overlay menu available\n\
                   00:00:04 - SDL Error (0): Connection terminated: -1\n\
                   00:00:04 - Qt Critical: Connection terminated\n\
                   \n\
                   Error code: -1\n\
                   00:00:05 - Qt Info: Session finished\n";
        assert_eq!(
            reason_from_log(log).as_deref(),
            Some("Connection terminated. Error code: -1")
        );
    }

    #[test]
    fn without_a_dialog_the_library_error_will_do() {
        let log = "00:00:01 - Qt Warning: qt.qpa noise\n\
                   00:00:02 - SDL Error (0): Starting RTSP handshake failed: 10060\n";
        assert_eq!(
            reason_from_log(log).as_deref(),
            Some("Starting RTSP handshake failed: 10060")
        );
        assert_eq!(reason_from_log("00:00:01 - Qt Info: fine\n"), None);
    }

    #[test]
    fn the_banner_leads_to_the_log() {
        let dir = std::env::temp_dir().join("dusk-test-client-log");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Moonlight-1.log");
        std::fs::write(
            &path,
            "00:00:03 - Qt Critical: No video received from host.\n\nCheck your firewall.\n",
        )
        .unwrap();
        let stderr = format!("Redirecting log output to {}\n", path.display());
        assert_eq!(
            clean_stderr(&stderr),
            "No video received from host. Check your firewall."
        );
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_that_crashes_says_so() {
        let mut child = a_child_that("kill -SEGV $$");
        let err = Moonlight::wait_for_session(&mut child, None)
            .await
            .expect_err("a crash is a failure");
        assert!(matches!(&err, MoonlightError::Failed(m) if m == "The streaming client crashed."));
    }

    /// Regression test for a real hang: `moonlight quit` against a host that
    /// will not answer never returns, so an un-budgeted run would wedge the
    /// UI in "busy" forever. `sleep` stands in for that behaviour.
    #[tokio::test]
    async fn a_command_that_never_returns_is_given_up_on() {
        let (program, args) = HANGS;
        let moonlight = Moonlight::at(PathBuf::from(program));
        let result = moonlight.run(args, Duration::from_millis(250)).await;
        assert!(matches!(result, Err(MoonlightError::TimedOut)));
    }

    #[tokio::test]
    async fn a_command_that_answers_in_time_is_not_cut_off() {
        let (program, args) = PRINTS_READY;
        let moonlight = Moonlight::at(PathBuf::from(program));
        let out = moonlight
            .run(args, Duration::from_secs(10))
            .await
            .expect("completes");
        assert_eq!(out.trim(), "ready");
    }

    /// A child shaped exactly like [`Moonlight::stream`]'s: stderr on a pipe
    /// that nobody has read yet.
    #[cfg(unix)]
    fn a_child_that(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn")
    }

    /// The regression this exists for: a stream that fails after launch used
    /// to go unreported, because nothing ever read its stderr or looked at
    /// its exit status.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_that_fails_reports_why() {
        let mut child = a_child_that("echo 'Host is unreachable' 1>&2; exit 255");
        let err = Moonlight::wait_for_session(&mut child, None)
            .await
            .expect_err("a non-zero exit is a failure");
        assert!(matches!(&err, MoonlightError::Failed(m) if m == "Host is unreachable"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_the_person_ended_is_not_an_error() {
        let mut child = a_child_that("exit 0");
        assert!(Moonlight::wait_for_session(&mut child, None).await.is_ok());
    }

    /// moonlight-qt writes several lines per decoded frame, so an unread
    /// pipe is a fuse under any long session. Far more than a pipe's worth
    /// here, and only the reason at the end is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_torrent_of_output_neither_blocks_nor_is_hoarded() {
        let mut child = a_child_that(
            "awk 'BEGIN { while (i++ < 20000) print \"FFmpeg: chatter\" }' 1>&2; \
             echo 'Connection terminated' 1>&2; exit 255",
        );
        let err = Moonlight::wait_for_session(&mut child, None)
            .await
            .expect_err("still a failure");
        assert!(matches!(&err, MoonlightError::Failed(m) if m == "Connection terminated"));
    }

    /// Memory holds only the tail, but the disk gets the whole session: the
    /// stream that froze a machine left nothing behind but its last forty
    /// lines in a process that no longer existed.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_whole_session_goes_to_its_log() {
        let dir = super::super::stream_log::tests::Dir::new("session-log");
        let log = super::super::stream_log::StreamLog::create_in_for_tests(dir.path());
        let path = log.path().to_owned();
        let mut child = a_child_that(
            "echo 'Video stream is 3440x1440' 1>&2; \
             awk 'BEGIN { while (i++ < 500) print \"FFmpeg: chatter\" }' 1>&2; exit 0",
        );
        Moonlight::wait_for_session(&mut child, Some(log))
            .await
            .expect("a clean exit");

        // The writer finishes on its own once the session lets go of it.
        let mut text = String::new();
        for _ in 0..50 {
            text = std::fs::read_to_string(&path).unwrap();
            if text.lines().count() == 501 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(text.lines().count(), 501);
        assert!(text
            .lines()
            .next()
            .unwrap()
            .ends_with(" Video stream is 3440x1440"));
    }

    #[test]
    fn discovery_prefers_an_explicit_override() {
        // The override has to win over the fixed list, or a portable install
        // is unreachable. Pointed at a file that certainly exists.
        let real = a_real_file();
        std::env::set_var("DUSK_MOONLIGHT_BIN", &real);
        let found = Moonlight::discover(None).expect("override is a file");
        assert_eq!(found.binary, real);

        // And it stays honest about what it found: a path given by hand is
        // not evidence either way about the overlay.
        assert_eq!(found.kind, ClientKind::Unknown);
        assert_eq!(found.overlay(), None);

        std::env::remove_var("DUSK_MOONLIGHT_BIN");
    }

    /// The regression this whole change exists for: a packaged build looked
    /// only in a `cfg(debug_assertions)` sibling checkout, so every release
    /// fell through to a stock Moonlight and streamed without an overlay.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_bundled_client_is_found_in_the_resource_directory() {
        let dir = std::env::temp_dir().join("dusk-bundled-client-test");
        let client = dir.join(BUNDLED_CLIENT);
        std::fs::create_dir_all(client.parent().unwrap()).unwrap();
        std::fs::write(&client, b"not really Moonlight").unwrap();

        assert!(
            dusk_client(Some(&dir)).contains(&client),
            "the resource directory has to be searched, or a shipped client is invisible"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// macOS looks beside the executable instead, in Contents/Helpers, and
    /// must *not* look in the resource directory: Tauri's resource copier
    /// flattens the symlinks a Qt framework is built from, which produces a
    /// bundle that notarization rejects. Searching there anyway would make a
    /// broken copy win over a correct one.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_client_is_sought_beside_the_executable_not_in_resources() {
        let resources = std::env::temp_dir().join("dusk-macos-client-test");
        let found = dusk_client(Some(&resources));

        assert!(
            found.iter().any(|p| p.ends_with(HELPER_CLIENT)),
            "Contents/Helpers has to be searched: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.starts_with(&resources)),
            "the resource directory must not be searched on macOS: {found:?}"
        );
    }

    #[test]
    fn without_a_resource_directory_nothing_is_invented() {
        // Every candidate has to be one Dusk ships or one a developer
        // checked out — never a guess at some other Moonlight on the
        // machine, which is what the ordinary system search is for.
        for path in dusk_client(None) {
            // `#[cfg]` and not `cfg!`: the constants themselves only exist
            // on the platforms that use them, so a runtime check would still
            // have to name one that is not compiled. Windows caught this.
            #[cfg(target_os = "macos")]
            let shipped = path.ends_with(HELPER_CLIENT);
            #[cfg(not(target_os = "macos"))]
            let shipped = false;

            #[cfg(debug_assertions)]
            let checked_out = path.ends_with(BUILT_CLIENT);
            #[cfg(not(debug_assertions))]
            let checked_out = false;

            assert!(
                shipped || checked_out,
                "unexpected candidate with no resource directory: {path:?}"
            );
        }
    }

    #[test]
    fn the_overlay_is_claimed_only_for_dusks_own_client() {
        let kinds = [
            (ClientKind::Dusk, Some(true)),
            (ClientKind::System, Some(false)),
            (ClientKind::Unknown, None),
        ];
        for (kind, expected) in kinds {
            let client = Moonlight::new(PathBuf::from("/nowhere"), kind);
            assert_eq!(client.overlay(), expected);
        }
    }

    #[tokio::test]
    async fn a_stock_client_is_never_asked_to_add_a_host() {
        // Only Dusk's fork has the action. A stock client handed one would
        // start its whole interface instead, so it is not even asked.
        let client = Moonlight::new(a_real_file(), ClientKind::System);
        assert!(!client.supports_add_host().await);
        assert!(client.add_host.get().is_none(), "the client was run");
    }

    #[test]
    fn the_capability_line_is_found_in_version_output() {
        // Captured from the fork's `--version`.
        assert!(advertises_add_host(
            "Moonlight 6.1.0\nDusk fork: add-host available\n"
        ));
        assert!(advertises_add_host(
            "Moonlight 6.1.0\r\nDusk fork: add-host available\r\n"
        ));
        // A stock client, or a fork from before the action.
        assert!(!advertises_add_host("Moonlight 6.1.0\n"));
        assert!(!advertises_add_host(""));
    }

    #[test]
    fn add_host_comes_first_and_a_port_is_passed_as_one() {
        let cert = Path::new("/tmp/c.pem");
        assert_eq!(
            add_host_args("10.0.0.5:47989", "HOST-ID", "Desk", cert),
            [
                "add-host",
                "10.0.0.5",
                "--uuid=HOST-ID",
                "--name=Desk",
                "--server-cert=/tmp/c.pem",
                "--port=47989"
            ]
        );
        assert_eq!(add_host_args("10.0.0.5", "H", "D", cert).len(), 5);
        assert_eq!(port_part("[fe80::1]:48000"), Some(48000));
        assert_eq!(port_part("fe80::1"), None);
        assert_eq!(port_part("gaming-pc.local"), None);
    }

    #[test]
    fn the_forks_startup_lines_are_not_the_reason() {
        let stderr = "00:00:00 - Qt Info: Dusk fork: in-stream overlay menu available\n\
                      00:00:00 - Qt Info: Dusk fork: add-host available\n\
                      Not a PEM certificate: /tmp/x.pem\n";
        assert_eq!(clean_stderr(stderr), "Not a PEM certificate: /tmp/x.pem");
    }

    /// An executable script at a fresh path in the temp directory.
    ///
    /// Copied into place by a child process, so this process never holds the
    /// file open for writing. Writing it here, even under another name and
    /// renamed, lost a race: another test forking while the write was open
    /// hands its child a copy of that descriptor, and until the child execs,
    /// running the script fails with "text file busy". A rename keeps the
    /// same file, so it does not help; a copy made elsewhere does.
    #[cfg(unix)]
    fn a_script(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("dusk-fake-client-{name}-{}", std::process::id()));
        let source = dir.join(format!(
            "dusk-fake-client-{name}-{}.source",
            std::process::id()
        ));
        std::fs::write(&source, body).unwrap();
        let _ = std::fs::remove_file(&path);
        let installed = std::process::Command::new("install")
            .args(["-m", "0755"])
            .arg(&source)
            .arg(&path)
            .status()
            .unwrap();
        assert!(installed.success(), "could not install {}", path.display());
        let _ = std::fs::remove_file(&source);
        path
    }

    /// A stand-in client: a script that checks it was handed a certificate
    /// file and then exits with `code`, saying `says` on stderr.
    #[cfg(unix)]
    fn a_client_exiting(name: &str, code: i32, says: &str) -> Moonlight {
        let path = a_script(
            name,
            &format!(
                "#!/bin/sh\n\
                 [ \"$1\" = add-host ] && [ -f \"${{5#--server-cert=}}\" ] || exit 9\n\
                 echo '00:00:00 - Qt Info: Dusk fork: add-host available' 1>&2\n\
                 echo '{says}' 1>&2\n\
                 exit {code}\n"
            ),
        );
        Moonlight::new(path, ClientKind::Dusk)
    }

    /// A client that could not be asked is asked again next time, rather
    /// than taken for one without the action until Dusk restarts.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_version_check_is_not_remembered() {
        let ready =
            std::env::temp_dir().join(format!("dusk-fake-client-ready-{}", std::process::id()));
        let _ = std::fs::remove_file(&ready);
        let path = a_script(
            "version",
            &format!(
                "#!/bin/sh\n\
                 [ -f '{}' ] || exit 1\n\
                 echo 'Moonlight 6.1.0'\n\
                 echo 'Dusk fork: add-host available'\n",
                ready.display()
            ),
        );
        let client = Moonlight::new(path.clone(), ClientKind::Dusk);

        assert!(!client.supports_add_host().await);
        assert!(client.add_host.get().is_none(), "a failure was cached");

        std::fs::write(&ready, b"").unwrap();
        assert!(client.supports_add_host().await);
        assert_eq!(client.add_host.get(), Some(&true));

        let _ = std::fs::remove_file(&ready);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_open_client_is_told_apart_from_a_failure() {
        let cert = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

        let client = a_client_exiting("stored", 0, "");
        assert!(client.add_host("10.0.0.5", "H", "D", cert).await.is_ok());

        let client = a_client_exiting("open", 2, "Another Dusk Stream client is running");
        assert!(matches!(
            client.add_host("10.0.0.5", "H", "D", cert).await,
            Err(MoonlightError::ClientOpen)
        ));

        let client = a_client_exiting("failed", 1, "Cannot write the client settings");
        assert!(matches!(
            client.add_host("10.0.0.5", "H", "D", cert).await,
            Err(MoonlightError::Failed(m)) if m == "Cannot write the client settings"
        ));
        for name in ["stored", "open", "failed"] {
            let _ = std::fs::remove_file(
                std::env::temp_dir()
                    .join(format!("dusk-fake-client-{name}-{}", std::process::id())),
            );
        }
    }
}

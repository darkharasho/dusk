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

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Pairing waits on a person walking to another machine and typing four
/// digits, so it gets a generous budget.
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

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
    #[error("could not run Moonlight: {0}")]
    Spawn(String),
    #[error("Moonlight did not answer, so Dusk stopped waiting")]
    TimedOut,
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Moonlight {
    binary: PathBuf,
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
}

impl Moonlight {
    pub fn at(binary: PathBuf) -> Self {
        Self { binary }
    }

    /// The client Dusk will actually run. Which settings store is the right
    /// one to read depends on it — see [`crate::moonlight::stores_for`].
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    /// Find moonlight-qt, or `None` if it is not installed.
    ///
    /// `DUSK_MOONLIGHT_BIN` overrides the search, which is what makes a
    /// portable or Flatpak install usable without Dusk having to enumerate
    /// every packaging scheme.
    pub fn discover() -> Option<Self> {
        if let Some(path) = std::env::var_os("DUSK_MOONLIGHT_BIN") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(Self::at(path));
            }
            eprintln!("dusk: DUSK_MOONLIGHT_BIN is set but {path:?} is not a file");
        }

        // Dusk's own client wins over any system Moonlight. It is a fork
        // carrying the in-stream overlay, so picking up a stock install
        // instead does not fail loudly — it streams perfectly well and the
        // overlay is simply absent, which reads as a bug in Dusk.
        if let Some(path) = dusk_client().into_iter().find(|p| p.is_file()) {
            eprintln!("dusk: using Dusk's Moonlight client at {path:?}");
            return Some(Self::at(path));
        }

        let found = candidates().into_iter().find(|p| p.is_file());
        if let Some(path) = &found {
            eprintln!("dusk: using system Moonlight at {path:?}; the in-stream overlay will be unavailable");
        }
        found.map(Self::at)
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

    /// Ask the host to end whatever session is running.
    pub async fn quit(&self, host: &str) -> Result<(), MoonlightError> {
        self.run(&["quit", host], ACTION_TIMEOUT).await.map(drop)
    }

    /// Launch a stream. The child runs for the life of the session, so this
    /// hands it back rather than waiting on it.
    pub fn stream(
        &self,
        host: &str,
        app: &str,
        options: &StreamOptions,
    ) -> Result<Child, MoonlightError> {
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

        Command::new(&self.binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
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
    pub async fn wait_for_session(child: &mut Child) -> Result<(), MoonlightError> {
        let drain = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL);
                while let Ok(Some(line)) = lines.next_line().await {
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
            Ok(())
        } else {
            Err(MoonlightError::Failed(clean_stderr(&stderr)))
        }
    }

    async fn run(&self, args: &[&str], budget: Duration) -> Result<String, MoonlightError> {
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

        let output = match tokio::time::timeout(budget, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(MoonlightError::Spawn(e.to_string())),
            Err(_) => return Err(MoonlightError::TimedOut),
        };

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        Err(MoonlightError::Failed(clean_stderr(
            &String::from_utf8_lossy(&output.stderr),
        )))
    }
}

/// Turn moonlight-qt's stderr into something worth showing a person.
///
/// Drops the log-redirection banner it prints unconditionally, and keeps the
/// last real line — the earlier ones are Qt plumbing, the last is the reason.
fn clean_stderr(stderr: &str) -> String {
    let message = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Redirecting log output"))
        .unwrap_or("");

    if message.is_empty() {
        "Moonlight failed but gave no reason.".into()
    } else {
        message.to_string()
    }
}

/// The forked client Dusk ships, relative to a build or install root.
#[cfg(target_os = "macos")]
const CLIENT_RELATIVE: &str = "app/Moonlight.app/Contents/MacOS/Moonlight";
#[cfg(target_os = "linux")]
const CLIENT_RELATIVE: &str = "app/moonlight";
#[cfg(target_os = "windows")]
const CLIENT_RELATIVE: &str = "app\\release\\Moonlight.exe";

/// Where Dusk's own Moonlight client might be.
///
/// Two places, and both are real. Installed, it sits beside the Dusk
/// executable because that is where the installer puts it. In development
/// it is a sibling checkout of the Dusk repository, which is the layout the
/// fork is cloned into — without that, a dev build silently falls through
/// to whatever Moonlight is installed system-wide and the overlay appears
/// to be broken when it is merely absent.
fn dusk_client() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(CLIENT_RELATIVE));
            out.push(dir.join("moonlight-qt").join(CLIENT_RELATIVE));
        }
    }

    // Development only: a release build must never reach outside itself.
    #[cfg(debug_assertions)]
    {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(workspace) = manifest.parent().and_then(|p| p.parent()) {
            out.push(workspace.join("moonlight-qt").join(CLIENT_RELATIVE));
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
            "Moonlight failed but gave no reason."
        );
        assert_eq!(clean_stderr(""), "Moonlight failed but gave no reason.");
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
        let err = Moonlight::wait_for_session(&mut child)
            .await
            .expect_err("a non-zero exit is a failure");
        assert!(matches!(&err, MoonlightError::Failed(m) if m == "Host is unreachable"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_the_person_ended_is_not_an_error() {
        let mut child = a_child_that("exit 0");
        assert!(Moonlight::wait_for_session(&mut child).await.is_ok());
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
        let err = Moonlight::wait_for_session(&mut child)
            .await
            .expect_err("still a failure");
        assert!(matches!(&err, MoonlightError::Failed(m) if m == "Connection terminated"));
    }

    #[test]
    fn discovery_prefers_an_explicit_override() {
        // The override has to win over the fixed list, or a portable install
        // is unreachable. Pointed at a file that certainly exists.
        let real = a_real_file();
        std::env::set_var("DUSK_MOONLIGHT_BIN", &real);
        let found = Moonlight::discover().expect("override is a file");
        assert_eq!(found.binary, real);
        std::env::remove_var("DUSK_MOONLIGHT_BIN");
    }
}

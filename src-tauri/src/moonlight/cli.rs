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
use std::path::{Path, PathBuf};
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
    /// A client at a known path. Only tests construct one this way —
    /// everything else arrives through [`Moonlight::discover`], which is also
    /// what establishes the [`ClientKind`].
    #[cfg(test)]
    pub fn at(binary: PathBuf) -> Self {
        Self {
            binary,
            kind: ClientKind::Unknown,
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
                return Some(Self {
                    binary: path,
                    kind: ClientKind::Unknown,
                });
            }
            eprintln!("dusk: DUSK_MOONLIGHT_BIN is set but {path:?} is not a file");
        }

        // Dusk's own client wins over any system Moonlight. It is a fork
        // carrying the in-stream overlay, so picking up a stock install
        // instead does not fail loudly — it streams perfectly well and the
        // overlay is simply absent, which reads as a bug in Dusk.
        if let Some(path) = dusk_client(resources).into_iter().find(|p| p.is_file()) {
            eprintln!("dusk: using Dusk's Moonlight client at {path:?}");
            return Some(Self {
                binary: path,
                kind: ClientKind::Dusk,
            });
        }

        let found = candidates().into_iter().find(|p| p.is_file());
        if let Some(path) = &found {
            eprintln!("dusk: using system Moonlight at {path:?}; the in-stream overlay will be unavailable");
        }
        found.map(|binary| Self {
            binary,
            kind: ClientKind::System,
        })
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
            let client = Moonlight {
                binary: PathBuf::from("/nowhere"),
                kind,
            };
            assert_eq!(client.overlay(), expected);
        }
    }
}

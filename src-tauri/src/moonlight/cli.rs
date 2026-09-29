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

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

/// Pairing waits on a person walking to another machine and typing four
/// digits, so it gets a generous budget.
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

/// Everything else talks to a host that is either there or not. Measured
/// against moonlight-qt 6.x, `quit` against a host that will not answer
/// hangs indefinitely rather than failing — hence a timeout at all.
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);

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

        candidates().into_iter().find(|p| p.is_file()).map(Self::at)
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
        let moonlight = Moonlight::at(PathBuf::from("/bin/sleep"));
        let result = moonlight.run(&["60"], Duration::from_millis(250)).await;
        assert!(matches!(result, Err(MoonlightError::TimedOut)));
    }

    #[tokio::test]
    async fn a_command_that_answers_in_time_is_not_cut_off() {
        let moonlight = Moonlight::at(PathBuf::from("/bin/echo"));
        let out = moonlight
            .run(&["ready"], Duration::from_secs(5))
            .await
            .expect("completes");
        assert_eq!(out.trim(), "ready");
    }

    #[test]
    fn discovery_prefers_an_explicit_override() {
        // The override has to win over the fixed list, or a portable install
        // is unreachable. Pointed at a file that certainly exists.
        std::env::set_var("DUSK_MOONLIGHT_BIN", "/bin/sh");
        let found = Moonlight::discover().expect("override is a file");
        assert_eq!(found.binary, PathBuf::from("/bin/sh"));
        std::env::remove_var("DUSK_MOONLIGHT_BIN");
    }
}

//! Running the service-control tools each platform ships.
//!
//! Every backend below shells out rather than linking a management API. That
//! is a deliberate trade: `launchctl`, `systemctl` and `sc` are stable,
//! scriptable, and identical to what a person would type, which makes a
//! failure something the user can reproduce and understand. Linking SCM on
//! Windows would buy better error codes and cost that.

use std::process::Stdio;
use std::time::Duration;

use super::HostError;

/// Service tools answer fast or they are stuck. Long enough for a cold
/// `sc query`, short enough that the UI is never wedged on one.
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Run {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }

    /// The most useful line to show a person when something failed.
    pub fn message(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("no output")
            .to_string()
    }
}

pub async fn run(program: &str, args: &[&str]) -> Result<Run, HostError> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| HostError::Failed(format!("could not run {program}: {e}")))?;

    let output = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(HostError::Failed(e.to_string())),
        Err(_) => {
            return Err(HostError::Failed(format!(
                "{program} did not answer, so Dusk stopped waiting"
            )))
        }
    };

    Ok(Run {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The launchd domain for the logged-in user, as `gui/<uid>`.
///
/// Sunshine installs as a *user agent* rather than a system daemon, so every
/// `launchctl` verb has to name that domain — a bare label resolves to
/// nothing and the command fails in a way that reads like "not installed".
#[cfg(unix)]
pub fn gui_domain_or_default() -> String {
    // Safety: getuid reads a process property. It cannot fail and touches no
    // memory we own.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Only ever called on macOS; defined for other targets so `platform.rs`
/// keeps compiling everywhere, which is what stops the backends rotting.
#[cfg(not(unix))]
pub fn gui_domain_or_default() -> String {
    "gui/0".to_string()
}

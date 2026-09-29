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

/// Anything elevated waits on a person reading a UAC prompt before the work
/// even begins, so the unelevated budget above would cut the prompt off
/// rather than the command.
pub const ELEVATED_TIMEOUT: Duration = Duration::from_secs(180);

/// An installer can legitimately run for minutes.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);

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
    run_for(program, args, TIMEOUT).await
}

pub async fn run_for(program: &str, args: &[&str], budget: Duration) -> Result<Run, HostError> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| HostError::Failed(format!("could not run {program}: {e}")))?;

    let output = match tokio::time::timeout(budget, child.wait_with_output()).await {
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

/// Run a program with administrator rights.
///
/// A process that is already running unelevated cannot elevate itself, so
/// this relaunches the target through PowerShell's `RunAs` verb, which is
/// what raises the UAC prompt.
///
/// Three details, each of which silently breaks this if missed:
///
/// - **`-PassThru` and an explicit `exit`.** Without them the exit code
///   belongs to PowerShell, which succeeds as long as it managed to *start*
///   the process. A failed install would report success.
/// - **`sc.exe`, never `sc`.** In PowerShell `sc` is an alias for
///   `Set-Content`, so the bare name silently runs something else entirely.
///   Callers pass the full executable name and this does not fix it up for
///   them, because the same trap applies to anything else they pass.
/// - **No output comes back.** An elevated child runs in a different
///   session and its streams are not ours to read, so the exit code is the
///   only signal. `Run::message` says so rather than reporting "no output".
#[cfg(target_os = "windows")]
pub async fn run_elevated(
    program: &str,
    args: &[&str],
    budget: Duration,
) -> Result<Run, HostError> {
    let script = elevation_script(program, args);

    let run = run_for(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        budget,
    )
    .await?;

    Ok(Run {
        status: run.status,
        stdout: run.stdout,
        // A cancelled UAC prompt surfaces here as a PowerShell error; keep
        // it, since it is the only explanation the user will get.
        stderr: run.stderr,
    })
}

/// Present on every platform so callers compile everywhere; only Windows
/// has a notion of relaunching a separate process to elevate it. Unix asks
/// per-command instead, which is what `osascript` and `sudo` are for.
#[cfg(not(target_os = "windows"))]
pub async fn run_elevated(
    _program: &str,
    _args: &[&str],
    _budget: Duration,
) -> Result<Run, HostError> {
    Err(HostError::Failed(
        "Relaunching a process with elevated rights is a Windows notion.".into(),
    ))
}

/// Single-quote for PowerShell, where a single quote is escaped by doubling
/// it. Without this, a path containing a quote would end the string and the
/// rest would be parsed as code.
pub fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The exact PowerShell the elevation path runs.
///
/// Split out from [`run_elevated`] so it can be asserted on from a machine
/// that is not Windows — which is the only way any of this gets checked
/// before it reaches one.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn elevation_script(program: &str, args: &[&str]) -> String {
    let arg_list = if args.is_empty() {
        String::new()
    } else {
        format!(
            " -ArgumentList {}",
            args.iter()
                .map(|a| powershell_quote(a))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    format!(
        "$p = Start-Process {}{} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        powershell_quote(program),
        arg_list
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_childs_exit_code_is_propagated_not_powershells() {
        // Without -PassThru and the explicit exit, PowerShell's own success
        // at *starting* the process would be reported as the result, and a
        // failed install would look like a completed one.
        let script = elevation_script("msiexec.exe", &["/i", "C:\\a.msi", "/qb"]);
        assert!(script.contains("-PassThru"), "{script}");
        assert!(script.ends_with("exit $p.ExitCode"), "{script}");
        assert!(script.contains("-Wait"), "{script}");
    }

    #[test]
    fn the_service_tool_is_named_with_its_extension() {
        // In PowerShell, `sc` is an alias for Set-Content. Passing the bare
        // name would quietly run the wrong program, so callers must pass
        // sc.exe and the quoting must preserve it.
        let script = elevation_script("sc.exe", &["start", "SunshineService"]);
        assert!(script.contains("'sc.exe'"), "{script}");
        assert!(script.contains("'start','SunshineService'"), "{script}");
    }

    #[test]
    fn arguments_are_quoted_individually_so_spaces_do_not_split_them() {
        let script = elevation_script("netsh.exe", &["advfirewall", "name=Sunshine (inbound TCP)"]);
        assert!(
            script.contains("'advfirewall','name=Sunshine (inbound TCP)'"),
            "{script}"
        );
    }

    #[test]
    fn a_quote_in_a_path_cannot_break_out_of_the_script() {
        // The one character that can end PowerShell single-quoting.
        let script = elevation_script("msiexec.exe", &[r"C:\it's\a.msi"]);
        assert!(script.contains(r"'C:\it''s\a.msi'"), "{script}");
        // And nothing was left able to run as a separate statement.
        assert_eq!(script.matches(';').count(), 1, "{script}");
    }

    #[test]
    fn a_program_with_no_arguments_omits_the_argument_list() {
        // Start-Process rejects an empty -ArgumentList.
        let script = elevation_script("foo.exe", &[]);
        assert!(!script.contains("-ArgumentList"), "{script}");
    }

    #[test]
    fn elevated_work_is_given_far_longer_than_an_ordinary_query() {
        // The unelevated budget would cut off a UAC prompt before the
        // command it guards even started.
        assert!(ELEVATED_TIMEOUT > TIMEOUT * 10);
        assert!(INSTALL_TIMEOUT > ELEVATED_TIMEOUT);
    }
}

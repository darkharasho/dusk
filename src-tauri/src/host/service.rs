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
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // `sc` and `powershell` are console programs, and Dusk is not a console
    // app, so Windows gives each one a console of its own — a window that
    // opens and shuts on every call. The status probe runs on the poll tick,
    // which turns that into a steady flashing. Nothing here is interactive
    // and the pipes above carry the output, so the console is pure artefact.
    #[cfg(target_os = "windows")]
    {
        // tokio's own method, not `std::os::windows::process::CommandExt`.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command
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

/// A script as PowerShell's `-EncodedCommand` wants it: base64 of its
/// UTF-16LE bytes. The one way to hand PowerShell a script through
/// `Start-Process` without its argument joining mangling the quoting.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn encode_powershell(script: &str) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
        // One pre-joined command line, not a PowerShell array. Given an array
        // `Start-Process` joins the elements with spaces and quotes nothing,
        // so an argument containing one — `C:\Program Files\...` — arrives at
        // the child split into several. Quoting each element for *PowerShell*
        // does not help: those quotes are consumed parsing the array literal
        // and never reach the command line. So the Windows quoting is applied
        // here, and the result handed over as a single string.
        format!(" -ArgumentList {}", powershell_quote(&command_line(args)))
    };
    format!(
        "$p = Start-Process {}{} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        powershell_quote(program),
        arg_list
    )
}

/// Join arguments the way `CommandLineToArgvW` will take them apart again.
///
/// Only arguments that need it are quoted, so a command line stays readable
/// in Task Manager and in anything a person is asked to reproduce by hand.
fn command_line(args: &[&str]) -> String {
    args.iter()
        .map(|a| quote_argument(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One argument, quoted for the Windows command-line parser.
///
/// The backslash rules are the awkward part: a run of backslashes is literal
/// on its own, but doubles when it precedes a quote — including the closing
/// one this adds. Without that, a path ending in a separator (`C:\dir\`)
/// would escape its own closing quote and swallow the next argument.
fn quote_argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push(c);
            }
            '"' => {
                // Double the run that precedes the quote, then escape it.
                out.extend(std::iter::repeat('\\').take(backslashes + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // The closing quote is a quote too, so any trailing run doubles as well.
    out.extend(std::iter::repeat('\\').take(backslashes));
    out.push('"');
    out
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
        assert!(script.contains("'start SunshineService'"), "{script}");
    }

    #[test]
    fn an_installer_path_containing_a_space_reaches_msiexec_whole() {
        // The bug this guards: Dusk's own installer puts the staged Sunshine
        // under `C:\Program Files\Dusk\resources`, so *every* Windows install
        // went through here. Handed to Start-Process as an array the path
        // arrived as `C:\Program`, and msiexec answered "this installation
        // package could not be opened".
        let msi = r"C:\Program Files\Dusk\resources\sunshine\sunshine.msi";
        let script = elevation_script("msiexec.exe", &["/i", msi, "/qb"]);
        assert!(
            script.contains(&format!("'/i \"{msi}\" /qb'")),
            "the path must reach msiexec as one argument: {script}"
        );
    }

    #[test]
    fn only_arguments_that_need_quoting_get_it() {
        assert_eq!(command_line(&["start", "SunshineService"]), "start SunshineService");
        assert_eq!(command_line(&["a b", "c"]), r#""a b" c"#);
    }

    #[test]
    fn a_trailing_separator_does_not_escape_its_own_closing_quote() {
        // `"C:\dir\"` would leave the quote escaped and swallow whatever
        // followed; the run before a quote has to double.
        assert_eq!(command_line(&[r"C:\a dir\", "/qb"]), r#""C:\a dir\\" /qb"#);
    }

    #[test]
    fn an_embedded_quote_is_escaped_rather_than_ending_the_argument() {
        assert_eq!(command_line(&[r#"a "b" c"#]), r#""a \"b\" c""#);
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
    fn an_encoded_command_is_base64_of_utf16le() {
        // What `[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('exit 0'))`
        // prints, padding included.
        assert_eq!(encode_powershell("exit 0"), "ZQB4AGkAdAAgADAA");
        assert_eq!(encode_powershell("a"), "YQA=");
        assert_eq!(encode_powershell("ab"), "YQBiAA==");
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

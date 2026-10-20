//! Per-platform host backends.
//!
//! All three compile on all three platforms even though only one is ever
//! constructed. Cross-compiling for real would catch more, but keeping them
//! type-checked everywhere stops the two you are not sitting in front of from
//! rotting between releases.
#![allow(dead_code)]

use async_trait::async_trait;

use super::service::{self, Run};
use super::{HostBackend, HostError};
use crate::install::embedded;
use crate::model::{HostCapabilities, HostPlatform, HostStatus, SupportTier};
use crate::sunshine::{provision, Credentials};

// ------------------------------------------------------------------ windows

/// Sunshine installs a system service under this name on Windows.
const WINDOWS_SERVICE: &str = "SunshineService";

pub struct WindowsHost;

#[async_trait]
impl HostBackend for WindowsHost {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Windows
    }

    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            support_tier: SupportTier::Supported,
            virtual_display: true,
            gamepad_input: true,
            system_audio: true,
            automated_setup: true,
            caveats: vec!["Starting and stopping the service needs administrator rights.".into()],
        }
    }

    async fn probe(&self) -> Result<HostStatus, HostError> {
        let run = service::run("sc", &["query", WINDOWS_SERVICE]).await?;

        // 1060 is "the specified service does not exist", which is how a
        // machine without Sunshine answers — not a failure.
        if !run.ok() {
            if run.stdout.contains("1060") || run.stderr.contains("1060") {
                return Ok(HostStatus::NotInstalled);
            }
            return Ok(HostStatus::Unknown {
                reason: run.message(),
            });
        }

        Ok(HostStatus::Installed {
            version: None, // Comes from the config API once credentials exist.
            running: parse_sc_running(&run.stdout),
        })
    }

    // Querying a service is unprivileged; starting and stopping one is not,
    // so these two elevate and `probe` does not.
    async fn start(&self) -> Result<(), HostError> {
        elevated_sc("start").await
    }

    async fn stop(&self) -> Result<(), HostError> {
        elevated_sc("stop").await
    }

    // Sunshine's credentials file lives beside it under Program Files, so
    // writing it needs administrator rights, and so does the restart. One
    // elevated script does both, so it costs one prompt rather than three.
    async fn set_credentials(&self, credentials: &Credentials) -> Result<(), HostError> {
        let program = windows_sunshine_exe().ok_or_else(|| {
            HostError::Failed("Dusk could not find sunshine.exe on this machine.".into())
        })?;
        let script = windows_creds_script(&program.to_string_lossy(), credentials);
        let run = service::run_elevated(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &service::encode_powershell(&script),
            ],
            service::ELEVATED_TIMEOUT,
        )
        .await?;
        if run.ok() {
            Ok(())
        } else {
            Err(HostError::Failed(
                "Windows did not let Dusk set up hosting.".into(),
            ))
        }
    }
}

/// Where the official installer puts Sunshine.
fn windows_sunshine_exe() -> Option<std::path::PathBuf> {
    std::env::var_os("ProgramFiles")
        .map(|base| std::path::PathBuf::from(base).join("Sunshine\\sunshine.exe"))
        .filter(|p| p.is_file())
}

/// Set the credential, then restart the service so it is read.
///
/// Passed to PowerShell encoded rather than as arguments: `Start-Process`
/// joins an argument list with spaces and quotes nothing, so a script given
/// as an argument through the elevation hop arrives split at every space.
fn windows_creds_script(program: &str, credentials: &Credentials) -> String {
    let [flag, user, pass] = provision::creds_args(credentials);
    format!(
        "& {} {} {} {}\n\
         if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\n\
         Restart-Service -Name {} -ErrorAction Stop\n\
         exit 0",
        service::powershell_quote(program),
        flag,
        service::powershell_quote(user),
        service::powershell_quote(pass),
        WINDOWS_SERVICE,
    )
}

async fn elevated_sc(verb: &str) -> Result<(), HostError> {
    // `sc.exe` rather than `sc`: PowerShell aliases the bare name to
    // Set-Content, which would silently do something else entirely.
    let run = service::run_elevated(
        "sc.exe",
        &[verb, WINDOWS_SERVICE],
        service::ELEVATED_TIMEOUT,
    )
    .await?;

    if run.ok() {
        return Ok(());
    }

    // An elevated child's output is not ours to read, so the exit code is
    // all there is. These two are worth naming; anything else falls through
    // to whatever PowerShell said.
    Err(HostError::Failed(match run.status {
        // 1060: the service is not installed.
        Some(1060) => "Hosting is not installed as a service on this machine.".into(),
        // 5: access denied, which here means the prompt was refused.
        Some(5) => "Windows did not grant permission to change the service.".into(),
        _ => {
            let said = run.message();
            if said == "no output" {
                format!("Windows could not {verb} the hosting service.")
            } else {
                said
            }
        }
    }))
}

/// `sc query` prints a `STATE` line whose numeric code is the reliable part;
/// the word beside it is localised on non-English Windows.
fn parse_sc_running(stdout: &str) -> bool {
    stdout
        .lines()
        .find(|line| line.trim_start().starts_with("STATE"))
        .and_then(|line| line.split(':').nth(1))
        .map(|rest| rest.split_whitespace().next().unwrap_or(""))
        .is_some_and(|code| code == "4")
}

// -------------------------------------------------------------------- linux

/// The user units a Sunshine install may have put on a Linux machine, best
/// first.
///
/// `sunshine` is what a distro package or a hand-written unit uses. The
/// second is the Flatpak's, which ships its unit under its application id —
/// so looking only for `sunshine` reports a Flatpak Sunshine as *not
/// installed*, which on an immutable distribution is the only Sunshine there
/// can be. Both are user units: the system manager knows neither.
const LINUX_UNITS: [&str; 2] = ["sunshine", "app-dev.lizardbyte.app.Sunshine"];

pub struct LinuxHost;

#[async_trait]
impl HostBackend for LinuxHost {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Linux
    }

    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            support_tier: SupportTier::Supported,
            // A dummy plug or a kernel option, not a driver Dusk can install.
            virtual_display: false,
            gamepad_input: true,
            system_audio: true,
            automated_setup: true,
            caveats: vec![
                "Input needs a udev rule for /dev/uinput.".into(),
                "Capture differs across X11, Wayland and KMS.".into(),
                "A virtual display needs a dummy plug or a kernel option.".into(),
            ],
        }
    }

    async fn probe(&self) -> Result<HostStatus, HostError> {
        let Some(unit) = installed_unit().await else {
            return Ok(HostStatus::NotInstalled);
        };

        // `is-active` exits non-zero when inactive, which is information
        // rather than an error — so the stdout word is what we read.
        let active = service::run("systemctl", &["--user", "is-active", unit]).await?;

        Ok(HostStatus::Installed {
            version: None,
            running: active.stdout.trim() == "active",
        })
    }

    async fn start(&self) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        expect(service::run("systemctl", &["--user", "start", unit]).await?)
    }

    async fn stop(&self) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        expect(service::run("systemctl", &["--user", "stop", unit]).await?)
    }

    async fn set_credentials(&self, credentials: &Credentials) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        let creds = provision::creds_args(credentials);

        let run = if unit == LINUX_UNITS[1] {
            // A Flatpak's binary is only reachable inside its sandbox, and
            // only there does it see the configuration the service uses.
            let mut args = vec!["run", "--command=sunshine", FLATPAK_ID];
            args.extend(creds);
            service::run("flatpak", &args).await?
        } else {
            // The unit's own binary, not whatever `sunshine` is on PATH: a
            // second copy would write a credentials file the service never
            // reads.
            let show = service::run(
                "systemctl",
                &["--user", "show", "-p", "ExecStart", "--value", unit],
            )
            .await?;
            let program = exec_start_path(&show.stdout).ok_or_else(|| {
                HostError::Failed(format!("Dusk could not tell which program {unit} runs."))
            })?;
            service::run(&program, &creds).await?
        };
        expect(run)?;

        expect(service::run("systemctl", &["--user", "restart", unit]).await?)
    }
}

const FLATPAK_ID: &str = "dev.lizardbyte.app.Sunshine";

/// The program in `systemctl show -p ExecStart --value`, which reads
/// `{ path=/usr/bin/sunshine ; argv[]=... ; ... }`.
fn exec_start_path(show: &str) -> Option<String> {
    show.split(';')
        .map(|part| part.trim().trim_start_matches('{').trim())
        .find_map(|part| part.strip_prefix("path="))
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

/// Which of [`LINUX_UNITS`] this machine actually has, if any.
///
/// Resolved per call rather than cached, so Sunshine installed while Dusk is
/// running is picked up on the next tick instead of at the next launch.
async fn installed_unit() -> Option<&'static str> {
    for unit in LINUX_UNITS {
        // Sunshine ships a *user* unit, so every call needs --user; the
        // system manager does not know these units exist.
        let known = service::run("systemctl", &["--user", "cat", unit]).await;
        if known.is_ok_and(|run| run.ok()) {
            return Some(unit);
        }
    }
    None
}

fn not_installed() -> HostError {
    HostError::Failed("Hosting is not installed as a user service on this machine.".into())
}

// -------------------------------------------------------------------- macos

/// Sunshine runs as a launchd **user agent** on macOS, not a system daemon.
/// Its plist sets `KeepAlive`, which is why stopping means `bootout` rather
/// than killing the process — launchd would just start it again.
const MACOS_LABEL: &str = "dev.lizardbyte.sunshine";

pub struct MacosHost;

impl MacosHost {
    fn app_bundles() -> Vec<std::path::PathBuf> {
        let mut out = vec![std::path::PathBuf::from("/Applications/Sunshine.app")];
        if let Some(home) = std::env::var_os("HOME") {
            out.push(std::path::PathBuf::from(home).join("Applications/Sunshine.app"));
        }
        out
    }

    /// The Sunshine the launch agent runs, if there is an agent and what it
    /// names still exists — which is how the helper inside Dusk is found,
    /// wherever Dusk is. Otherwise a Sunshine.app installed the usual way.
    fn installed_bundle() -> Option<std::path::PathBuf> {
        let from_agent = Self::agent_plist()
            .and_then(|p| embedded::agent_program(&p))
            .filter(|p| p.is_file())
            .and_then(|p| embedded::bundle_of(&p));
        from_agent.or_else(|| Self::app_bundles().into_iter().find(|p| p.is_dir()))
    }

    fn agent_plist() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        let path = std::path::PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{MACOS_LABEL}.plist"));
        path.is_file().then_some(path)
    }

    /// The bundle's own version. Read from disk because the config API needs
    /// credentials Dusk may not have yet, and a version is worth showing
    /// before anyone signs in.
    fn bundle_version() -> Option<String> {
        embedded::version(&Self::installed_bundle()?)
    }
}

#[async_trait]
impl HostBackend for MacosHost {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Macos
    }

    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            // Sunshine's own tier, not ours: macOS hosting is experimental
            // upstream. Streaming *to* a Mac is unaffected.
            support_tier: SupportTier::Experimental,
            virtual_display: false,
            gamepad_input: false,
            system_audio: false,
            // Screen Recording and Accessibility are per-app grants in System
            // Settings. No installer can script them.
            automated_setup: false,
            caveats: vec![
                "Hosting from a Mac is experimental.".into(),
                "Screen Recording and Accessibility must be granted by hand.".into(),
                "Controllers are not supported when hosting from a Mac.".into(),
                "System audio needs a loopback device such as BlackHole.".into(),
            ],
        }
    }

    async fn probe(&self) -> Result<HostStatus, HostError> {
        let Some(_) = Self::installed_bundle() else {
            return Ok(HostStatus::NotInstalled);
        };

        // `launchctl list <label>` prints a plist-ish dict and exits 0 when
        // the job is loaded. A loaded-but-stopped job has no PID key, which
        // is the difference between "installed" and "running".
        let run = service::run("launchctl", &["list", MACOS_LABEL]).await?;
        let running = run.ok() && run.stdout.contains("\"PID\"");

        Ok(HostStatus::Installed {
            version: Self::bundle_version(),
            running,
        })
    }

    async fn start(&self) -> Result<(), HostError> {
        let domain = service::gui_domain_or_default();
        let loaded = service::run("launchctl", &["list", MACOS_LABEL])
            .await?
            .ok();

        // A job that was booted out is gone from the domain entirely, so
        // kickstart would fail with "no such process" — it has to be
        // bootstrapped back from its plist first.
        if !loaded {
            let Some(plist) = Self::agent_plist() else {
                return Err(HostError::Failed(
                    "The hosting launch agent is missing, so Dusk cannot start it.".into(),
                ));
            };
            let out = service::run(
                "launchctl",
                &["bootstrap", &domain, &plist.to_string_lossy()],
            )
            .await?;
            if !out.ok() {
                return Err(HostError::Failed(out.message()));
            }
        }

        expect(
            service::run(
                "launchctl",
                &["kickstart", &format!("{domain}/{MACOS_LABEL}")],
            )
            .await?,
        )
    }

    async fn stop(&self) -> Result<(), HostError> {
        let domain = service::gui_domain_or_default();
        expect(
            service::run(
                "launchctl",
                &["bootout", &format!("{domain}/{MACOS_LABEL}")],
            )
            .await?,
        )
    }

    async fn set_credentials(&self, credentials: &Credentials) -> Result<(), HostError> {
        let bundle = Self::installed_bundle()
            .ok_or_else(|| HostError::Failed("Hosting is not installed on this Mac.".into()))?;
        let program = embedded::program(&bundle);
        expect(
            service::run(
                &program.to_string_lossy(),
                &provision::creds_args(credentials),
            )
            .await?,
        )?;

        // `-k` kills a running job before starting it, which is a restart;
        // a stopped one simply starts, which is also fine.
        let domain = service::gui_domain_or_default();
        expect(
            service::run(
                "launchctl",
                &["kickstart", "-k", &format!("{domain}/{MACOS_LABEL}")],
            )
            .await?,
        )
    }
}

fn expect(run: Run) -> Result<(), HostError> {
    if run.ok() {
        Ok(())
    } else {
        Err(HostError::Failed(run.message()))
    }
}

pub fn current() -> Box<dyn HostBackend> {
    #[cfg(target_os = "windows")]
    return Box::new(WindowsHost);
    #[cfg(target_os = "macos")]
    return Box::new(MacosHost);
    #[cfg(target_os = "linux")]
    return Box::new(LinuxHost);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sc_state_is_read_from_the_code_not_the_word() {
        // The word beside the code is localised; the number is not.
        assert!(parse_sc_running(
            "        STATE              : 4  RUNNING \n"
        ));
        assert!(!parse_sc_running(
            "        STATE              : 1  STOPPED \n"
        ));
        // A localised Windows still parses, because only the code is read.
        assert!(parse_sc_running(
            "        STATE              : 4  EN COURS \n"
        ));
        assert!(!parse_sc_running("no state line here"));
    }

    #[test]
    fn the_units_own_binary_is_read_from_exec_start() {
        let show = "{ path=/home/linuxbrew/.linuxbrew/opt/sunshine/bin/sunshine ; \
                    argv[]=/home/linuxbrew/.linuxbrew/opt/sunshine/bin/sunshine ; \
                    ignore_errors=no ; pid=5767 ; code=(null) ; status=0/0 }";
        assert_eq!(
            exec_start_path(show).as_deref(),
            Some("/home/linuxbrew/.linuxbrew/opt/sunshine/bin/sunshine")
        );
        assert_eq!(exec_start_path(""), None);
    }

    #[test]
    fn the_windows_script_sets_then_restarts_and_stops_on_failure() {
        let c = Credentials {
            username: "dusk".into(),
            password: "Abc123".into(),
        };
        let script = windows_creds_script(r"C:\Program Files\Sunshine\sunshine.exe", &c);
        let set = script.find("--creds 'dusk' 'Abc123'").expect(&script);
        let check = script.find("$LASTEXITCODE").expect(&script);
        let restart = script
            .find("Restart-Service -Name SunshineService")
            .expect(&script);
        assert!(set < check && check < restart, "{script}");
        assert!(
            script.starts_with(r"& 'C:\Program Files\Sunshine\sunshine.exe'"),
            "{script}"
        );
    }

    #[test]
    fn a_pending_state_is_not_running() {
        // 2 is START_PENDING — treating it as running would show hosting as
        // on before it can accept a connection.
        assert!(!parse_sc_running(
            "        STATE              : 2  START_PENDING \n"
        ));
    }
}

/// Exercises the real Linux backend against this machine's Sunshine unit.
///
/// Ignored: it needs Sunshine installed as a user unit, and it stops and
/// starts the service, so it is not something to run under a stream. It
/// restores whatever state it found.
#[cfg(all(test, target_os = "linux"))]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn probe_start_and_stop_agree_with_systemd() {
        let host = LinuxHost;

        let before = host.probe().await.expect("probe");
        println!("probe: {before:?}");
        let was_running = match before {
            HostStatus::Installed { running, .. } => running,
            other => panic!("Sunshine is not installed as a user unit here: {other:?}"),
        };

        host.stop().await.expect("stop");
        let stopped = host.probe().await.expect("probe after stop");
        println!("after stop: {stopped:?}");
        assert!(
            matches!(stopped, HostStatus::Installed { running: false, .. }),
            "a stopped unit must read as not running, got {stopped:?}"
        );

        host.start().await.expect("start");
        let started = host.probe().await.expect("probe after start");
        println!("after start: {started:?}");
        assert!(
            matches!(started, HostStatus::Installed { running: true, .. }),
            "a started unit must read as running, got {started:?}"
        );

        if !was_running {
            host.stop().await.expect("restore");
        }
    }
}

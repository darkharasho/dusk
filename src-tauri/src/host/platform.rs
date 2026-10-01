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
use crate::model::{HostCapabilities, HostPlatform, HostStatus, SupportTier};

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
        Some(1060) => "Sunshine is not installed as a service on this machine.".into(),
        // 5: access denied, which here means the prompt was refused.
        Some(5) => "Windows did not grant permission to change the service.".into(),
        _ => {
            let said = run.message();
            if said == "no output" {
                format!("Windows could not {verb} the Sunshine service.")
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
    HostError::Failed("Sunshine is not installed as a user service on this machine.".into())
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

    fn installed_bundle() -> Option<std::path::PathBuf> {
        Self::app_bundles().into_iter().find(|p| p.is_dir())
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
    #[cfg(target_os = "macos")]
    fn bundle_version() -> Option<String> {
        let bundle = Self::installed_bundle()?;
        let value = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
        value
            .as_dictionary()?
            .get("CFBundleShortVersionString")?
            .as_string()
            .map(str::to_string)
    }

    #[cfg(not(target_os = "macos"))]
    fn bundle_version() -> Option<String> {
        None
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
                "Sunshine treats macOS hosting as experimental.".into(),
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
                    "Sunshine's launch agent is missing, so Dusk cannot start it.".into(),
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

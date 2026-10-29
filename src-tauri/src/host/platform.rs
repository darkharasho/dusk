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
use crate::model::{HostCapabilities, HostPlatform, HostProvenance, HostStatus, SupportTier};
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
            sized_screen: true,
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
            // Sunshine's own installer registers `SunshineService` whether
            // Dusk ran it or a person did, so there is nothing here to tell
            // the two apart.
            provenance: HostProvenance::Unknown,
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
            // No virtual display here — no driver Dusk can install, and
            // `amdgpu` has no VIRTUAL output — but the real output's mode
            // can be changed around a stream; see `host::screen_mode`.
            sized_screen: true,
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
            version: unit_version(unit).await,
            running: active.stdout.trim() == "active",
            provenance: unit_provenance(unit).await,
        })
    }

    async fn start(&self) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        expect(
            service::run_for(
                "systemctl",
                &["--user", "start", unit],
                service::LIFECYCLE_TIMEOUT,
            )
            .await?,
        )
    }

    async fn stop(&self) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        expect(
            service::run_for(
                "systemctl",
                &["--user", "stop", unit],
                service::LIFECYCLE_TIMEOUT,
            )
            .await?,
        )
    }

    async fn set_credentials(&self, credentials: &Credentials) -> Result<(), HostError> {
        let unit = installed_unit().await.ok_or_else(not_installed)?;
        let creds = provision::creds_args(credentials);

        let run = if unit == LINUX_UNITS[1] {
            // A Flatpak's binary is only reachable inside its sandbox, and
            // only there does it see the configuration the service uses.
            let mut args = vec!["run", "--command=sunshine", FLATPAK_ID];
            args.extend(creds);
            service::run_for("flatpak", &args, service::LIFECYCLE_TIMEOUT).await?
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

        expect(
            service::run_for(
                "systemctl",
                &["--user", "restart", unit],
                service::LIFECYCLE_TIMEOUT,
            )
            .await?,
        )
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
/// Where both exist the newer wins. It used to be whichever came first in
/// [`LINUX_UNITS`], which on the Bazzite machine meant driving the image's
/// 2026.516 while a 2026.914 Flatpak sat installed and unused beside it.
async fn installed_unit() -> Option<&'static str> {
    let mut present = Vec::new();
    for unit in LINUX_UNITS {
        // Sunshine ships a *user* unit, so every call needs --user; the
        // system manager does not know these units exist.
        let known = service::run("systemctl", &["--user", "cat", unit]).await;
        if known.is_ok_and(|run| run.ok()) {
            present.push(unit);
        }
    }

    // One unit is the common case, and asking a host its version costs a
    // subprocess — so only a genuine ambiguity pays for the comparison.
    if present.len() < 2 {
        return present.first().copied();
    }

    let mut best: Option<(&'static str, Vec<u64>)> = None;
    for &unit in &present {
        let Some(key) = unit_version(unit).await.map(|v| version_key(&v)) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((_, found)) => key > *found,
        };
        if better {
            best = Some((unit, key));
        }
    }

    // A version nothing would report falls back to the list's order, so a
    // host that will not answer behaves exactly as it always did.
    best.map(|(unit, _)| unit)
        .or_else(|| present.first().copied())
}

/// Whether the host running here is sandboxed. A sandbox decides for itself
/// which devices it can see, and the seat's permissions do not reach inside
/// it — so this changes what a `/dev/uinput` rule is worth.
pub async fn linux_host_sandboxed() -> bool {
    match installed_unit().await {
        None => false,
        Some(unit) if unit == LINUX_UNITS[1] => true,
        // A hand-written unit that runs `flatpak run …` is the same
        // situation wearing the other unit's name.
        Some(unit) => unit_program(unit)
            .await
            .is_some_and(|program| program == "flatpak" || program.ends_with("/flatpak")),
    }
}

/// Whether the sandboxed host has been handed every device.
///
/// `all` is the only permission that includes `/dev/uinput`. `input` sounds
/// like the one that would and does not: it covers `/dev/input` and stops
/// there.
pub async fn linux_sandbox_grants_devices() -> bool {
    let shown = service::run("flatpak", &["info", "--show-permissions", FLATPAK_ID]).await;
    shown.is_ok_and(|run| {
        run.ok()
            && run.stdout.lines().any(|line| {
                let line = line.trim();
                line.starts_with("devices")
                    && line
                        .split_once('=')
                        .is_some_and(|(_, list)| list.split(';').any(|dev| dev.trim() == "all"))
            })
    })
}

/// What Dusk writes into the unit it registers — see
/// `install::apply::user_unit`. Comparing it is the only way to tell Dusk's
/// own service from one that was already on the machine, because both are
/// called `sunshine.service` and live in the same directory.
const DUSK_UNIT_DESCRIPTION: &str = "Sunshine, managed by Dusk";

/// Whether this is the service Dusk registered, or one it deferred to.
async fn unit_provenance(unit: &str) -> HostProvenance {
    let shown = service::run(
        "systemctl",
        &["--user", "show", "-p", "Description", "--value", unit],
    )
    .await;

    match shown {
        Ok(run) if run.ok() => {
            if run.stdout.trim() == DUSK_UNIT_DESCRIPTION {
                HostProvenance::Dusk
            } else {
                HostProvenance::Foreign
            }
        }
        // Not knowing is its own answer: claiming `Foreign` here would put a
        // warning in front of someone whose host is fine.
        _ => HostProvenance::Unknown,
    }
}

/// The `ExecStart` program of a unit, which is the host binary Dusk should
/// ask about rather than whatever `sunshine` is on `PATH`. On the Bazzite
/// machine those were two different Sunshines, five months apart.
async fn unit_program(unit: &str) -> Option<String> {
    let show = service::run(
        "systemctl",
        &["--user", "show", "-p", "ExecStart", "--value", unit],
    )
    .await
    .ok()?;
    exec_start_path(&show.stdout)
}

/// The version of the Sunshine a unit actually runs.
///
/// Cached, because `probe` runs on every poller tick and this costs starting
/// Sunshine far enough for it to print. The key carries the program's mtime,
/// so an upgrade in place is picked up rather than remembered wrong.
async fn unit_version(unit: &str) -> Option<String> {
    let program = if unit == LINUX_UNITS[1] {
        None
    } else {
        Some(unit_program(unit).await?)
    };

    let key = match &program {
        Some(path) => format!("{path}@{}", mtime_key(path)),
        None => format!("flatpak:{FLATPAK_ID}"),
    };
    if let Some(hit) = version_cache().lock().await.get(&key) {
        return Some(hit.clone());
    }

    // Asking Sunshine its version makes it open its log, which truncates the
    // log the running service has open — so the child is pointed at a
    // directory of its own. Without this, reading the version on a poll tick
    // destroys the history anyone would need to diagnose that same host.
    let scratch = version_scratch_dir();
    let scratch = scratch.to_string_lossy();
    let env = [
        ("XDG_CONFIG_HOME", scratch.as_ref()),
        ("XDG_STATE_HOME", scratch.as_ref()),
        // `HOME` too: a build that resolves its config without the XDG
        // variables lands here rather than in the real one.
        ("HOME", scratch.as_ref()),
    ];

    let run = match &program {
        // A Flatpak's binary is only reachable inside its sandbox, the same
        // reason `set_credentials` goes through `flatpak run`.
        None => {
            service::run_with_env(
                "flatpak",
                &["run", "--command=sunshine", FLATPAK_ID, "--version"],
                &env,
                service::TIMEOUT,
            )
            .await
        }
        Some(path) => service::run_with_env(path, &["--version"], &env, service::TIMEOUT).await,
    }
    .ok()?;

    // Deliberately not gated on `run.ok()`: the text is what was wanted and
    // some builds exit non-zero after printing it.
    let found = parse_version(&run.stdout).or_else(|| parse_version(&run.stderr))?;

    // Only successes are remembered. A host mid-install answers nothing, and
    // caching that would keep it blank until Dusk restarted.
    version_cache().lock().await.insert(key, found.clone());
    Some(found)
}

/// A throwaway config directory for the version probe, so the host's own one
/// is never written to. Created if it is missing; its contents do not matter
/// and are never read.
fn version_scratch_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("dusk-host-version");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

type VersionCache = tokio::sync::Mutex<std::collections::HashMap<String, String>>;

fn version_cache() -> &'static VersionCache {
    static CACHE: std::sync::OnceLock<VersionCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn mtime_key(path: &str) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The version out of `sunshine --version`.
///
/// Sunshine logs its whole configuration before it says what it is, so the
/// line is matched rather than the first one taken.
fn parse_version(out: &str) -> Option<String> {
    out.lines()
        .filter_map(|line| line.split_once("Sunshine version:"))
        .find_map(|(_, rest)| rest.split_whitespace().next())
        .map(str::to_string)
}

/// A dotted numeric version, comparable. `v2026.914.233613` and
/// `2026.914.233613` are one version written two ways.
pub fn version_key(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
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

    /// Whether the bundle being driven is the helper Dusk's own launch agent
    /// points at, or a Sunshine.app that was already installed.
    ///
    /// Only `install::embedded` ever writes that agent, so its presence is
    /// the signal. This is the case `docs/pickup.md:14` recorded: a Mac
    /// running a standalone 2026.516 from `/Applications` that Dusk drove
    /// without ever saying it was not the one Dusk ships.
    fn provenance() -> HostProvenance {
        let from_agent = Self::agent_plist()
            .and_then(|plist| embedded::agent_program(&plist))
            .is_some_and(|program| program.is_file());

        if from_agent {
            HostProvenance::Dusk
        } else {
            HostProvenance::Foreign
        }
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
            sized_screen: false,
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
            provenance: Self::provenance(),
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
            let out = service::run_for(
                "launchctl",
                &["bootstrap", &domain, &plist.to_string_lossy()],
                service::LIFECYCLE_TIMEOUT,
            )
            .await?;
            if !out.ok() {
                return Err(HostError::Failed(out.message()));
            }
        }

        expect(
            service::run_for(
                "launchctl",
                &["kickstart", &format!("{domain}/{MACOS_LABEL}")],
                service::LIFECYCLE_TIMEOUT,
            )
            .await?,
        )
    }

    async fn stop(&self) -> Result<(), HostError> {
        let domain = service::gui_domain_or_default();
        expect(
            service::run_for(
                "launchctl",
                &["bootout", &format!("{domain}/{MACOS_LABEL}")],
                service::LIFECYCLE_TIMEOUT,
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
            service::run_for(
                "launchctl",
                &["kickstart", "-k", &format!("{domain}/{MACOS_LABEL}")],
                service::LIFECYCLE_TIMEOUT,
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
    fn a_version_is_found_past_the_configuration_sunshine_prints_first() {
        // Measured output from the Bazzite host, 2026-10-29. Sunshine logs
        // its whole config before it says what it is, and an earlier line
        // contains a number — so taking the first line, or the first
        // number, both give the wrong answer.
        let out = "\
[2026-10-29 11:51:07.751562] [info]    config: 'output_name' = 0\n\
[2026-10-29 11:51:07.753]: Info: Sunshine version: 2026.516.143833 commit: 14ffa6fd\n\
[2026-10-29 11:51:07.753]: Info: Package Publisher: LizardByte\n";
        assert_eq!(parse_version(out).as_deref(), Some("2026.516.143833"));
        assert_eq!(parse_version("nothing of the sort\n"), None);
    }

    #[test]
    fn the_newer_of_two_hosts_is_the_bigger_key() {
        // The pair actually installed on the Bazzite machine: the image's
        // Sunshine and an unused Flatpak five months newer. Picking by list
        // order drove the older one.
        assert!(version_key("2026.914.233613") > version_key("2026.516.143833"));
        // The tag Dusk pins is the same version wearing a `v`.
        assert_eq!(
            version_key("v2026.914.233613"),
            version_key("2026.914.233613")
        );
        // A version that will not parse sorts below one that does, rather
        // than panicking or winning.
        assert!(version_key("2026.914") > version_key("what"));
    }

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

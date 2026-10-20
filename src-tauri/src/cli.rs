//! Setting a machine up without a screen in front of it.
//!
//! The window is the ordinary way in, and on a desktop it should stay that
//! way. But a machine you only ever stream *from* often has nobody sitting
//! at it: a box in a cupboard, a headless Linux host, a machine reached over
//! SSH. Before this, setting one up meant a logged-in desktop session and a
//! mouse, because every step lived behind a button.
//!
//! Nothing here reimplements a step. The work list comes from
//! [`install::steps::build`] — the same call the checklist screen makes — and
//! each step runs the same function the button runs, so the two cannot drift
//! into disagreeing about what setting a machine up means.
//!
//! This runs *before* Tauri starts, like the elevated driver helper, so no
//! window is ever created and no display is needed.

use std::path::PathBuf;

use crate::host::{self, HostBackend};
use crate::install::{
    self,
    steps::{Step, StepId, StepState},
};
use crate::model::HostStatus;
use crate::sunshine::CredentialStore;

const USAGE: &str = "\
Dusk — set this machine up to host, without opening the window.

USAGE:
    dusk setup     Carry out every setup step Dusk can do by itself
    dusk status    Print what this machine's hosting looks like

SETUP OPTIONS:
    --unlock-keyring   Unlock this machine's keyring first, reading the
                       password from standard input. For a machine that logs
                       in automatically, whose keyring nothing has opened.
                       Lasts until the next restart; its password is left as
                       it is.
    --store=tpm        Keep Dusk's secrets sealed to this machine's TPM
                       instead of in the keyring, so they survive a restart
                       with nobody there to unlock anything. Refuses if there
                       is no TPM to seal to.

Run dusk with no arguments to open the window as usual.
";

/// What `dusk setup` was asked to do besides the steps themselves.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    unlock_keyring: bool,
    seal: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    for arg in args {
        match arg.as_str() {
            "--unlock-keyring" => options.unlock_keyring = true,
            "--store=tpm" => options.seal = true,
            // Named rather than ignored: a misspelt flag that silently did
            // nothing would look like the flag not working.
            other => return Err(format!("dusk setup does not understand {other}")),
        }
    }
    Ok(options)
}

/// Handle a subcommand if one was given, and return the exit code.
///
/// `None` means no subcommand, so the caller should start the app normally.
/// Matching only the first argument keeps this out of the way of the flags
/// the window understands, such as `--background`.
pub fn run_if_asked() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = args.first()?;
    match arg.as_str() {
        "setup" => match parse(&args[1..]) {
            Ok(options) => Some(block_on(setup(options))),
            Err(err) => {
                eprintln!("{err}\n\n{USAGE}");
                Some(2)
            }
        },
        "status" => Some(block_on(status())),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Some(0)
        }
        // Anything else is for the window — `--background`, a URL, whatever
        // the platform appends — and is not ours to reject.
        _ => None,
    }
}

/// The password for `--unlock-keyring`, from standard input.
///
/// Standard input and nothing else. As an argument it would be visible to
/// anything that can run `ps`; in an environment variable it would be
/// readable from `/proc`. A pipe is the one channel that is neither.
fn read_password() -> Result<String, String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the password from standard input ({e})"))?;
    let password = line.trim_end_matches(['\n', '\r']).to_string();
    if password.is_empty() {
        return Err("no password arrived on standard input".to_string());
    }
    Ok(password)
}

#[cfg(target_os = "linux")]
fn unlock_keyring(password: &str) -> Result<(), String> {
    crate::secrets::unlock::login_keyring(password)
}

#[cfg(not(target_os = "linux"))]
fn unlock_keyring(_password: &str) -> Result<(), String> {
    Err(
        "Only Linux keeps its keystore behind a lock a session has to open; \
         this platform's needs no unlocking."
            .to_string(),
    )
}

/// Run an async body without Tauri's runtime, which is not up yet.
fn block_on<F: std::future::Future<Output = i32>>(future: F) -> i32 {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(future),
        Err(err) => {
            eprintln!("dusk: could not start: {err}");
            1
        }
    }
}

async fn status() -> i32 {
    let host = host::detect();
    let state = host.state().await;
    let credentials = CredentialStore::default();
    let managed = credentials.load().await.is_some();

    println!("Platform:  {:?}", state.platform);
    match &state.status {
        HostStatus::NotInstalled => println!("Hosting:   not installed"),
        HostStatus::Installed { version, running } => println!(
            "Hosting:   installed{}, {}",
            version
                .as_deref()
                .map(|v| format!(" (version {v})"))
                .unwrap_or_default(),
            if *running { "running" } else { "stopped" }
        ),
        HostStatus::Unknown { reason } => println!("Hosting:   unknown — {reason}"),
    }
    println!(
        "Managed:   {}",
        if managed {
            "yes, Dusk holds this host's sign-in"
        } else {
            "no, Dusk has no sign-in for this host"
        }
    );
    println!("Secrets:   {}", crate::secrets::backend().describe());
    if let Some(warning) = keyring_warning() {
        println!("           {warning}");
    }
    0
}

/// Said plainly, because the failure it describes is otherwise silent: the
/// machine looks set up, and forgets who it is at the next restart.
fn keyring_warning() -> Option<String> {
    if crate::secrets::backend() != crate::secrets::Backend::Keystore {
        return None;
    }
    #[cfg(target_os = "linux")]
    if crate::secrets::unlock::login_keyring_locked().unwrap_or(false) {
        return Some(
            "this machine's keyring is locked, so nothing can be kept across a restart.\n           \
             Run `dusk setup --unlock-keyring` for this boot, or `--store=tpm` for good."
                .to_string(),
        );
    }
    None
}

async fn setup(options: Options) -> i32 {
    // Both of these decide *where* a secret will land, so they happen before
    // any step that might write one. Provisioning a host and then finding
    // the answer went somewhere unintended is not recoverable by rerunning:
    // the password is already Sunshine's.
    if options.unlock_keyring {
        match read_password() {
            Ok(password) => {
                if let Err(err) = unlock_keyring(&password) {
                    eprintln!("dusk: could not unlock the keyring: {err}");
                    return 1;
                }
                println!("• Unlocked this machine's keyring");
            }
            Err(err) => {
                eprintln!("dusk: {err}");
                return 1;
            }
        }
    }
    if options.seal {
        match crate::secrets::enable_sealed() {
            Ok(dir) => println!(
                "• Sealing Dusk's secrets to this machine ({})",
                dir.display()
            ),
            Err(err) => {
                eprintln!("dusk: {err}");
                return 1;
            }
        }
    }

    let host = host::detect();
    let credentials = CredentialStore::default();

    // The same loop the checklist's one button runs: re-read after each
    // step, because one step often finishes another — installing also sets
    // up the sign-in, which also starts hosting. `tried` is what stops a
    // step that reports itself unfinished from looping forever.
    let mut tried: Vec<StepId> = Vec::new();
    loop {
        let setup = build(host.as_ref(), &credentials).await;
        let Some(step) = next_step(&setup.steps, &tried) else {
            break;
        };
        tried.push(step.id);

        println!("• {}", step.title);
        if let Err(err) = perform(step.id, host.as_ref(), &credentials).await {
            eprintln!("  failed: {err}");
            report(&build(host.as_ref(), &credentials).await.steps);
            return 1;
        }
    }

    let setup = build(host.as_ref(), &credentials).await;
    report(&setup.steps);

    // A host that was set up but whose sign-in cannot outlive the process is
    // the failure this whole path exists to stop being silent. Hosting works,
    // so it is not an error — but it is never nothing.
    if !credentials.is_persistent().await {
        eprintln!(
            "\nWARNING: Dusk could not keep this host's sign-in.\n\
             It works now and is forgotten at the next restart, so this machine\n\
             will not be one your others recognise.\n\
             Fix it with `dusk setup --unlock-keyring` (this boot) or\n\
             `dusk setup --store=tpm` (for good)."
        );
    }

    // What is left that Dusk could never do itself is not a failure: a
    // firewall it cannot identify, a permission only a person can grant.
    // Exiting non-zero for those would make every headless run look broken.
    if setup.steps.iter().any(unfinished_and_automatable) {
        eprintln!("\nSome steps Dusk can do are still unfinished.");
        return 1;
    }
    0
}

async fn build(host: &dyn HostBackend, credentials: &CredentialStore) -> install::Setup {
    let state = host.state().await;
    install::steps::build(
        state.platform,
        &state.status,
        state.capabilities.support_tier,
        credentials.load().await.is_some(),
        install::apply::linux_input_ready(),
        install::display_driver::installed(),
    )
}

/// The next step Dusk may carry out unasked.
///
/// Optional steps are skipped on purpose: the virtual display installs a
/// driver, and a driver should never arrive as a side effect of a command
/// that was asked to set hosting up.
fn next_step<'a>(steps: &'a [Step], tried: &[StepId]) -> Option<&'a Step> {
    steps.iter().find(|s| {
        s.automatable && !s.optional && s.state == StepState::Todo && !tried.contains(&s.id)
    })
}

fn unfinished_and_automatable(step: &Step) -> bool {
    step.automatable && !step.optional && step.state == StepState::Todo
}

async fn perform(
    id: StepId,
    host: &dyn HostBackend,
    credentials: &CredentialStore,
) -> Result<(), String> {
    match id {
        StepId::InstallSunshine => install_hosting().await,
        StepId::StartService => host.start().await.map_err(|e| e.to_string()),
        StepId::Provision => crate::sunshine::provision::run(host, credentials).await,
        StepId::Input => install::apply::linux_allow_input()
            .await
            .map_err(|e| e.to_string()),
        // Automatable on Windows only; elsewhere the checklist marks it
        // Unknown and `next_step` never picks it.
        StepId::Firewall => crate::commands::open_firewall().await,
        // Reached only if `next_step` let one through, which would mean the
        // checklist called it automatable and this did not keep up.
        other => Err(format!("{other:?} is not a step dusk setup can carry out.")),
    }
}

/// Install the Sunshine that ships inside Dusk.
///
/// Only the bundled installer, never the download: a headless run has no way
/// to show what is about to be fetched or to ask about it, and the
/// alternative is a command that quietly pulls a release off the internet
/// and installs it with elevated rights.
async fn install_hosting() -> Result<(), String> {
    if let Some(helper) = install::embedded::find() {
        return install::apply::install_embedded(&helper)
            .await
            .map_err(|e| e.to_string());
    }
    let resources = resource_dir();
    let bundled = install::bundled::find(resources.as_deref()).ok_or_else(|| {
        "This build of Dusk carries no hosting installer, so it has to be installed from the window."
            .to_string()
    })?;
    install::bundled::verify(&bundled).await?;
    install::apply::install(&bundled.path, bundled.kind)
        .await
        .map_err(|e| e.to_string())
}

/// Where Tauri would have put the bundled installer, found without Tauri.
///
/// The window gets this from `app.path().resource_dir()`, which needs the
/// app to exist. These are the same three places that resolves to, each
/// checked for the manifest rather than assumed from the platform, so a
/// layout that moves is a miss rather than a wrong answer.
fn resource_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        // Linux: /usr/bin/dusk beside /usr/lib/Dusk
        dir.join("../lib/Dusk"),
        // Windows and a portable layout
        dir.to_path_buf(),
        // macOS: Contents/MacOS/dusk beside Contents/Resources
        dir.join("../Resources"),
    ]
    .into_iter()
    .find(|base| {
        base.join(install::bundled::DIR)
            .join("bundled.json")
            .is_file()
    })
}

fn report(steps: &[Step]) {
    println!();
    for step in steps {
        let mark = match &step.state {
            StepState::Done => "done",
            StepState::Todo if step.automatable && !step.optional => "TO DO",
            StepState::Todo => "optional",
            StepState::Unknown => "needs you",
            StepState::NotNeeded { .. } => "n/a",
        };
        println!("  [{mark:^9}] {}", step.title);
        if matches!(step.state, StepState::Unknown)
            || (step.state == StepState::Todo && !step.automatable)
        {
            println!("              {}", step.detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostPlatform, HostStatus, SupportTier};

    fn steps(status: HostStatus, managed: bool) -> Vec<Step> {
        install::steps::build(
            HostPlatform::Linux,
            &status,
            SupportTier::Supported,
            managed,
            true,
            false,
        )
        .steps
    }

    #[test]
    fn a_fresh_machine_starts_by_installing() {
        let s = steps(HostStatus::NotInstalled, false);
        assert_eq!(next_step(&s, &[]).unwrap().id, StepId::InstallSunshine);
    }

    #[test]
    fn a_step_already_attempted_is_not_offered_again() {
        // Without this the loop would reinstall forever on a machine where
        // installing does not make the status change.
        let s = steps(HostStatus::NotInstalled, false);
        let next = next_step(&s, &[StepId::InstallSunshine]).unwrap();
        assert_ne!(next.id, StepId::InstallSunshine);
    }

    #[test]
    fn an_installed_but_unmanaged_host_only_needs_the_sign_in() {
        let s = steps(
            HostStatus::Installed {
                version: None,
                running: true,
            },
            false,
        );
        assert_eq!(next_step(&s, &[]).unwrap().id, StepId::Provision);
    }

    #[test]
    fn a_finished_machine_has_nothing_left_to_do() {
        let s = steps(
            HostStatus::Installed {
                version: None,
                running: true,
            },
            true,
        );
        assert!(next_step(&s, &[]).is_none());
        assert!(!s.iter().any(unfinished_and_automatable));
    }

    #[test]
    fn the_virtual_display_is_never_installed_unasked() {
        // Windows is where the optional driver step exists. A command asked
        // to set hosting up must not install a display driver as well.
        let s = install::steps::build(
            HostPlatform::Windows,
            &HostStatus::Installed {
                version: None,
                running: true,
            },
            SupportTier::Supported,
            true,
            true,
            false,
        )
        .steps;
        assert!(s
            .iter()
            .any(|s| s.id == StepId::VirtualDisplay && s.state == StepState::Todo));

        // Walk the loop to exhaustion: the driver must never be the choice,
        // however many other steps are pending around it.
        let mut tried = Vec::new();
        while let Some(next) = next_step(&s, &tried) {
            assert_ne!(
                next.id,
                StepId::VirtualDisplay,
                "a driver must never install as a side effect"
            );
            tried.push(next.id);
        }
        assert!(
            tried.contains(&StepId::Firewall),
            "the firewall is Dusk's to do"
        );
    }

    #[test]
    fn a_grant_only_a_person_can_make_does_not_count_as_unfinished() {
        // macOS asks for screen recording and accessibility, which no
        // command can set. A headless run must not call those a failure.
        let s = install::steps::build(
            HostPlatform::Macos,
            &HostStatus::Installed {
                version: None,
                running: true,
            },
            SupportTier::Experimental,
            true,
            true,
            false,
        )
        .steps;
        assert!(s.iter().any(|s| s.state == StepState::Unknown));
        assert!(!s.iter().any(unfinished_and_automatable));
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn setup_takes_no_options_by_default() {
        // Neither flag may be reachable by accident: one wants a password,
        // the other moves where every secret on the machine is kept.
        assert_eq!(parse(&[]).unwrap(), Options::default());
        assert!(!Options::default().unlock_keyring);
        assert!(!Options::default().seal);
    }

    #[test]
    fn both_options_are_understood_together() {
        // The real headless case: unlock the keyring to get going, and seal
        // so the next restart does not need unlocking at all.
        let o = parse(&args(&["--unlock-keyring", "--store=tpm"])).unwrap();
        assert!(o.unlock_keyring && o.seal);
    }

    #[test]
    fn a_misspelt_option_is_refused_rather_than_ignored() {
        // Ignoring it would look exactly like the flag not working, on the
        // one command whose whole job is to be run unattended.
        assert!(parse(&args(&["--store=tmp"])).is_err());
        assert!(parse(&args(&["--unlock_keyring"])).is_err());
        assert!(parse(&args(&["--store=file"])).is_err());
    }

    #[test]
    fn only_the_known_subcommands_are_claimed() {
        // `--background` belongs to the window; claiming it here would stop
        // Dusk ever starting hidden.
        for arg in ["--background", "dusk://x", "-"] {
            assert!(
                !matches!(arg, "setup" | "status" | "help" | "--help" | "-h"),
                "{arg} must fall through to the window"
            );
        }
    }
}

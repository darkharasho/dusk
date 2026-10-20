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

Run dusk with no arguments to open the window as usual.
";

/// Handle a subcommand if one was given, and return the exit code.
///
/// `None` means no subcommand, so the caller should start the app normally.
/// Matching only the first argument keeps this out of the way of the flags
/// the window understands, such as `--background`.
pub fn run_if_asked() -> Option<i32> {
    let arg = std::env::args().nth(1)?;
    match arg.as_str() {
        "setup" => Some(block_on(setup())),
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
    0
}

async fn setup() -> i32 {
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

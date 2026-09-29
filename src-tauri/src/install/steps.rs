//! What first-run setup consists of, and where it stands.

use serde::{Deserialize, Serialize};

use crate::model::{HostPlatform, HostStatus, SupportTier};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepId {
    InstallSunshine,
    StartService,
    SignIn,
    Firewall,
    ScreenRecording,
    Accessibility,
    SystemAudio,
    VirtualDisplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum StepState {
    Done,
    Todo,
    /// Dusk cannot tell from here — a permission grant it is not allowed to
    /// read, for instance. The person confirms it themselves.
    Unknown,
    /// Not applicable on this machine, with the reason.
    NotNeeded { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: StepId,
    pub title: String,
    pub detail: String,
    pub state: StepState,
    /// True when Dusk can carry the step out itself. False means the person
    /// has to do it, and `detail` says what.
    pub automatable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setup {
    pub steps: Vec<Step>,
}

fn step(
    id: StepId,
    title: &str,
    detail: &str,
    state: StepState,
    automatable: bool,
) -> Step {
    Step {
        id,
        title: title.to_string(),
        detail: detail.to_string(),
        state,
        automatable,
    }
}

/// Build the checklist for this machine.
///
/// Steps that do not apply are kept and marked, rather than hidden: "not
/// needed here, and here is why" is more useful than a list that quietly
/// differs between machines.
pub fn build(
    platform: HostPlatform,
    status: &HostStatus,
    tier: SupportTier,
    signed_in: bool,
) -> Setup {
    let installed = !matches!(status, HostStatus::NotInstalled);
    let running = matches!(status, HostStatus::Installed { running: true, .. });

    let mut steps = vec![
        step(
            StepId::InstallSunshine,
            "Install Sunshine",
            "Dusk downloads the official release and checks it before installing.",
            if installed { StepState::Done } else { StepState::Todo },
            true,
        ),
        step(
            StepId::StartService,
            "Run Sunshine in the background",
            "So this machine can be reached without Sunshine being open.",
            match (installed, running) {
                (false, _) => StepState::Todo,
                (true, true) => StepState::Done,
                (true, false) => StepState::Todo,
            },
            true,
        ),
        step(
            StepId::SignIn,
            "Sign in to Sunshine",
            "Lets Dusk configure the host and accept pairing PINs for you.",
            if signed_in { StepState::Done } else { StepState::Todo },
            false,
        ),
    ];

    steps.push(firewall_step(platform));

    match platform {
        HostPlatform::Macos => {
            // These are TCC grants. No installer, elevated or not, can set
            // them — and Dusk cannot read them either without trying to
            // capture and seeing what happens.
            steps.push(step(
                StepId::ScreenRecording,
                "Allow screen recording",
                "Open System Settings › Privacy & Security › Screen Recording and turn on Sunshine.",
                StepState::Unknown,
                false,
            ));
            steps.push(step(
                StepId::Accessibility,
                "Allow accessibility control",
                "Open System Settings › Privacy & Security › Accessibility and turn on Sunshine. Without it the other machine cannot control this one.",
                StepState::Unknown,
                false,
            ));
            steps.push(step(
                StepId::SystemAudio,
                "Install a loopback audio device",
                "macOS will not let an app capture system sound. BlackHole is the usual answer.",
                StepState::Unknown,
                false,
            ));
        }
        HostPlatform::Windows => {
            steps.push(step(
                StepId::VirtualDisplay,
                "Install a virtual display",
                "Lets this machine be streamed at the other machine's resolution, and with no monitor attached.",
                StepState::Todo,
                true,
            ));
        }
        HostPlatform::Linux => {
            steps.push(step(
                StepId::VirtualDisplay,
                "Set up a virtual display",
                "Needs a dummy plug or a kernel option. Dusk cannot do this for you.",
                StepState::Unknown,
                false,
            ));
        }
        HostPlatform::Mock => {}
    }

    if tier == SupportTier::Experimental {
        for s in &mut steps {
            if s.id == StepId::VirtualDisplay {
                s.state = StepState::NotNeeded {
                    reason: "Not available when hosting from this platform.".into(),
                };
            }
        }
    }

    Setup { steps }
}

fn firewall_step(platform: HostPlatform) -> Step {
    match platform {
        // macOS prompts the first time Sunshine listens, and its firewall is
        // off by default, so there is nothing sensible to automate.
        HostPlatform::Macos => step(
            StepId::Firewall,
            "Allow Sunshine through the firewall",
            "macOS asks the first time Sunshine accepts a connection. Answer Allow.",
            StepState::Unknown,
            false,
        ),
        HostPlatform::Windows => step(
            StepId::Firewall,
            "Open the firewall for Sunshine",
            "Adds inbound rules for the ports Sunshine listens on.",
            StepState::Todo,
            true,
        ),
        HostPlatform::Linux => step(
            StepId::Firewall,
            "Open the firewall for Sunshine",
            "Only needed if you run one. Dusk cannot tell which.",
            StepState::Unknown,
            false,
        ),
        HostPlatform::Mock => step(
            StepId::Firewall,
            "Open the firewall for Sunshine",
            "Mock host.",
            StepState::Done,
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(running: bool) -> HostStatus {
        HostStatus::Installed {
            version: Some("2026.914".into()),
            running,
        }
    }

    #[test]
    fn a_fresh_machine_has_everything_to_do() {
        let setup = build(
            HostPlatform::Windows,
            &HostStatus::NotInstalled,
            SupportTier::Supported,
            false,
        );
        let install = setup
            .steps
            .iter()
            .find(|s| s.id == StepId::InstallSunshine)
            .unwrap();
        assert_eq!(install.state, StepState::Todo);
    }

    #[test]
    fn an_already_set_up_machine_shows_only_what_is_left() {
        // The common case: Sunshine installed and running, Dusk just not
        // signed in yet. A wizard would insist on walking the whole path.
        let setup = build(
            HostPlatform::Windows,
            &installed(true),
            SupportTier::Supported,
            false,
        );
        let by = |id: StepId| {
            setup.steps.iter().find(|s| s.id == id).unwrap().state.clone()
        };
        assert_eq!(by(StepId::InstallSunshine), StepState::Done);
        assert_eq!(by(StepId::StartService), StepState::Done);
        assert_eq!(by(StepId::SignIn), StepState::Todo);
    }

    #[test]
    fn macos_lists_the_grants_it_cannot_make_and_does_not_claim_them_done() {
        let setup = build(
            HostPlatform::Macos,
            &installed(true),
            SupportTier::Experimental,
            true,
        );
        for id in [StepId::ScreenRecording, StepId::Accessibility] {
            let s = setup.steps.iter().find(|s| s.id == id).unwrap();
            assert_eq!(s.state, StepState::Unknown);
            assert!(!s.automatable, "a TCC grant cannot be scripted");
        }
    }

    #[test]
    fn a_virtual_display_is_marked_unavailable_rather_than_pending() {
        // Leaving it Todo on a platform that cannot have one would be a
        // permanently unfinishable checklist.
        let setup = build(
            HostPlatform::Macos,
            &installed(true),
            SupportTier::Experimental,
            true,
        );
        let vd = setup.steps.iter().find(|s| s.id == StepId::VirtualDisplay);
        assert!(vd.is_none() || matches!(vd.unwrap().state, StepState::NotNeeded { .. }));
    }

    #[test]
    fn windows_offers_to_do_the_firewall_and_macos_does_not() {
        let win = firewall_step(HostPlatform::Windows);
        assert!(win.automatable);
        let mac = firewall_step(HostPlatform::Macos);
        assert!(!mac.automatable);
        assert_eq!(mac.state, StepState::Unknown);
    }
}

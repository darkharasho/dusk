//! Per-platform host backends.
//!
//! The capability matrices below are real and drive what the UI promises. The
//! `probe`/`start`/`stop` bodies are M3 work; until then they report Unknown
//! rather than pretending Sunshine is absent.
//!
//! All three backends compile on all three platforms even though only one is
//! ever constructed. Cross-compiling for real would catch more, but keeping
//! them type-checked everywhere stops the two you are not sitting in front of
//! from rotting between releases.
#![allow(dead_code)]

use super::{HostBackend, HostError};
use crate::model::{HostCapabilities, HostPlatform, HostStatus, SupportTier};

fn todo_m3() -> Result<HostStatus, HostError> {
    Ok(HostStatus::Unknown {
        reason: "Hosting cannot be set up from Dusk yet".into(),
    })
}

// ------------------------------------------------------------------ windows

pub struct WindowsHost;

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
            caveats: Vec::new(),
        }
    }

    fn probe(&self) -> Result<HostStatus, HostError> {
        todo_m3()
    }
    fn start(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
    }
    fn stop(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
    }
}

// -------------------------------------------------------------------- linux

pub struct LinuxHost;

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

    fn probe(&self) -> Result<HostStatus, HostError> {
        todo_m3()
    }
    fn start(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
    }
    fn stop(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
    }
}

// -------------------------------------------------------------------- macos

pub struct MacosHost;

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

    fn probe(&self) -> Result<HostStatus, HostError> {
        todo_m3()
    }
    fn start(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
    }
    fn stop(&self) -> Result<(), HostError> {
        Err(HostError::NotImplemented)
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

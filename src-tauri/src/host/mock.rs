//! An in-memory host backend.
//!
//! This is what makes the host-side UI developable on a machine that cannot
//! usefully run Sunshine. It reports a plausible capability matrix so the UI
//! is exercised against honest constraints.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use super::{HostBackend, HostError};
use crate::model::{HostCapabilities, HostPlatform, HostStatus, SupportTier};

pub struct MockHost {
    running: AtomicBool,
    installed: bool,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(true),
            installed: true,
        }
    }
}

#[async_trait]
impl HostBackend for MockHost {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Mock
    }

    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            support_tier: SupportTier::Supported,
            virtual_display: true,
            gamepad_input: true,
            system_audio: true,
            automated_setup: true,
            caveats: vec!["Mock host. Nothing here touches a real service.".into()],
        }
    }

    async fn probe(&self) -> Result<HostStatus, HostError> {
        if !self.installed {
            return Ok(HostStatus::NotInstalled);
        }
        Ok(HostStatus::Installed {
            version: Some("0.23.1".into()),
            running: self.running.load(Ordering::Relaxed),
        })
    }

    async fn start(&self) -> Result<(), HostError> {
        self.running.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.running.store(false, Ordering::Relaxed);
        Ok(())
    }
}

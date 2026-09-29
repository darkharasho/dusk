//! Platform-abstracted control of the local Sunshine host.
//!
//! The three platforms share almost nothing below this trait — a launchd
//! user agent, a systemd user unit and a Windows service, three capture
//! stacks, three installer stories — so the trait is deliberately narrow and
//! everything platform-shaped lives behind it.

use async_trait::async_trait;

use crate::model::{HostCapabilities, HostPlatform, HostState, HostStatus};

pub mod mock;
pub mod platform;
pub mod service;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("not supported on this platform")]
    Unsupported,
    #[error("{0}")]
    Failed(String),
}

/// `async_trait` rather than a plain `async fn`: every implementation waits
/// on a subprocess, and native async fns in traits are not object-safe, which
/// `Box<dyn HostBackend>` needs.
#[async_trait]
pub trait HostBackend: Send + Sync {
    fn platform(&self) -> HostPlatform;

    /// Static facts about what hosting can do here. Drives what the UI is
    /// willing to promise before anything is installed.
    fn capabilities(&self) -> HostCapabilities;

    /// Is Sunshine installed, and is it running right now?
    async fn probe(&self) -> Result<HostStatus, HostError>;

    async fn start(&self) -> Result<(), HostError>;

    async fn stop(&self) -> Result<(), HostError>;

    async fn state(&self) -> HostState {
        HostState {
            platform: self.platform(),
            capabilities: self.capabilities(),
            status: self.probe().await.unwrap_or_else(|e| HostStatus::Unknown {
                reason: e.to_string(),
            }),
        }
    }
}

/// Pick a backend for the machine we are running on.
///
/// `DUSK_MOCK=1` forces the mock backend, which is how the host-side UI stays
/// developable on a machine that cannot usefully run Sunshine.
pub fn detect() -> Box<dyn HostBackend> {
    if std::env::var("DUSK_MOCK").is_ok_and(|v| v == "1") {
        return Box::new(mock::MockHost::default());
    }
    platform::current()
}

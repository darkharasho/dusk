//! Platform-abstracted control of the local Sunshine host.
//!
//! The three platforms share almost nothing below this trait — SCM vs systemd
//! vs launchd, three capture stacks, three installer stories — so the trait is
//! deliberately narrow and everything platform-shaped lives behind it.
//!
//! M0 ships the capability matrices (which are real and load-bearing for the
//! UI) and a mock backend. The install/start/stop bodies land in M3.

use crate::model::{HostCapabilities, HostPlatform, HostState, HostStatus};

pub mod mock;
pub mod platform;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("not implemented for this platform yet")]
    NotImplemented,
    #[error("{0}")]
    Failed(String),
}

pub trait HostBackend: Send + Sync {
    fn platform(&self) -> HostPlatform;

    /// Static facts about what hosting can do here. Drives what the UI is
    /// willing to promise before anything is installed.
    fn capabilities(&self) -> HostCapabilities;

    /// Is Sunshine installed, and is it running right now?
    fn probe(&self) -> Result<HostStatus, HostError>;

    fn start(&self) -> Result<(), HostError>;

    fn stop(&self) -> Result<(), HostError>;

    fn state(&self) -> HostState {
        HostState {
            platform: self.platform(),
            capabilities: self.capabilities(),
            status: self.probe().unwrap_or_else(|e| HostStatus::Unknown {
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

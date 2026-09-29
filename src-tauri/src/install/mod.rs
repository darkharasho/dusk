//! First-run setup.
//!
//! The messy part of hosting: getting Sunshine onto the machine, registering
//! it as a service, opening the firewall, and — on macOS — walking someone
//! through the permission grants no installer is allowed to script.
//!
//! Setup is modelled as a checklist rather than a wizard. A wizard assumes
//! nothing is done yet, which is wrong for most people here: Sunshine is
//! often already installed, often already running, and the only outstanding
//! item might be a firewall rule. A checklist shows the state of each step
//! and lets any one of them be run on its own.

pub mod apply;
pub mod download;
pub mod release;
pub mod steps;

pub use steps::Setup;

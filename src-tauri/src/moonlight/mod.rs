//! The Moonlight client side.
//!
//! M2 shells out to moonlight-qt. The streaming logic lives in
//! moonlight-common-c, so an embedded renderer can replace the subprocess
//! later without the UI changing — see `identity` for what that swap costs
//! and why it is already paid for.

pub mod cli;
pub mod hosts;
pub mod identity;

pub use cli::Moonlight;
pub use identity::ClientIdentity;

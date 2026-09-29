//! The Sunshine host side.
//!
//! Sunshine stays a separate process that Dusk manages and configures over
//! its local API. That is what keeps Dusk's own source out of GPL-3.0
//! territory, and it is also why the installer fetches Sunshine at first run
//! rather than bundling it.

pub mod api;
pub mod credentials;

pub use api::{ApiError, SunshineApi};
pub use credentials::{CredentialStore, Credentials};

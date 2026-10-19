//! The Sunshine host side.
//!
//! Sunshine stays a separate process that Dusk manages and configures over
//! its local API. That is what keeps Dusk's own source out of GPL-3.0
//! territory: Dusk ships Sunshine's installer beside itself, but never links
//! it.

pub mod api;
pub mod credentials;
pub mod provision;

pub use api::{ApiError, SunshineApi};
pub use credentials::{CredentialStore, Credentials, Known};

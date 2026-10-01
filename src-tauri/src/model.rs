//! The device model the entire UI renders from.
//!
//! Kept in sync by hand with `src/types.ts`. Serde's representation *is* the
//! wire format, so changes here are breaking changes for the frontend.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type DeviceId = String;

/// Sunshine's plain-HTTP GameStream port. Unauthenticated; enough for
/// liveness and session state, not for pairing state.
pub const DEFAULT_HTTP_PORT: u16 = 47989;
/// Sunshine's TLS GameStream port. Pairing state requires a client
/// certificate presented here, which Dusk does not own until M2.
pub const DEFAULT_HTTPS_PORT: u16 = 47984;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Reachability {
    Online { rtt_ms: u32 },
    Offline,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PairingState {
    Paired,
    NotPaired,
    /// We cannot know without a client certificate. Say so rather than
    /// rendering an unpaired machine as definitively unpaired.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Activity {
    Idle,
    Hosting {
        app_id: Option<String>,
        /// Resolved from the host's app list in M2; None until then.
        app_name: Option<String>,
    },
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSource {
    pub mdns: bool,
    pub manual: bool,
    /// Remembered by moonlight-qt. Says the machine is known, not that it is
    /// reachable — which is the whole reason the source exists.
    pub moonlight: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDetails {
    pub hostname: Option<String>,
    pub mac: Option<String>,
    pub app_version: Option<String>,
    pub local_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    /// Every address we know for this machine. A machine on both LAN and VPN
    /// legitimately has more than one.
    pub addresses: Vec<String>,
    /// The address that last answered, and the one we try first.
    pub primary_address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub source: DeviceSource,
    pub is_self: bool,
    pub reachability: Reachability,
    pub pairing: PairingState,
    pub activity: Activity,
    pub server: Option<ServerDetails>,
    /// What this host offers to stream. Only populated once we are paired,
    /// because `applist` requires the client certificate.
    pub apps: Vec<crate::applist::HostApp>,
    /// True when *this* Dusk is streaming from the machine right now.
    ///
    /// Distinct from `activity`, which reports what Sunshine says about
    /// itself. A host holds a session open after its client disconnects —
    /// that is GameStream working as designed, not a stream you are in —
    /// so the card has to tell the two apart or it claims you are
    /// streaming when nothing is on screen.
    pub streaming_here: bool,
    pub last_seen_ms: Option<u64>,

    /// A name the user typed. Survives merges so discovery cannot overwrite
    /// the label someone chose. Internal to the backend.
    #[serde(skip)]
    pub custom_name: Option<String>,
}

impl Device {
    pub fn new(id: DeviceId, name: String) -> Self {
        Self {
            id,
            name,
            addresses: Vec::new(),
            primary_address: None,
            http_port: DEFAULT_HTTP_PORT,
            https_port: DEFAULT_HTTPS_PORT,
            source: DeviceSource::default(),
            is_self: false,
            reachability: Reachability::Unknown,
            pairing: PairingState::Unknown,
            activity: Activity::Unknown,
            server: None,
            apps: Vec::new(),
            streaming_here: false,
            last_seen_ms: None,
            custom_name: None,
        }
    }

    pub fn add_address(&mut self, address: &str) {
        if !self.addresses.iter().any(|a| a == address) {
            self.addresses.push(address.to_string());
        }
        if self.primary_address.is_none() {
            self.primary_address = Some(address.to_string());
        }
    }

    /// Addresses ordered so the one that last answered is tried first.
    pub fn probe_order(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for addr in self.primary_address.iter().chain(self.addresses.iter()) {
            if seen.insert(addr.clone()) {
                out.push(addr.clone());
            }
        }
        out
    }
}

// --------------------------------------------------------------------- host

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostPlatform {
    Windows,
    Linux,
    Macos,
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportTier {
    Supported,
    Experimental,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilities {
    /// Sunshine's own support tier for hosting on this platform, not ours.
    pub support_tier: SupportTier,
    pub virtual_display: bool,
    pub gamepad_input: bool,
    pub system_audio: bool,
    /// False where the OS requires a manual permission grant no installer can
    /// script, which on macOS is Screen Recording and Accessibility.
    pub automated_setup: bool,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostStatus {
    NotInstalled,
    Installed {
        version: Option<String>,
        running: bool,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostState {
    pub platform: HostPlatform,
    pub capabilities: HostCapabilities,
    pub status: HostStatus,
}

/// The single payload pushed to the frontend on every state change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub host: HostState,
    pub discovering: bool,
    /// False when moonlight-qt is not installed. Nothing on the client side
    /// works without it, so the UI says so rather than offering buttons that
    /// can only fail.
    pub moonlight_available: bool,
    /// Whether Dusk holds this machine's Sunshine sign-in. Everything on the
    /// host side beyond start/stop needs it.
    pub host_signed_in: bool,
    /// False when the sign-in is only held for this run because the OS
    /// keystore was unavailable. The UI says so rather than letting someone
    /// believe it was saved.
    pub host_credentials_persistent: bool,
}

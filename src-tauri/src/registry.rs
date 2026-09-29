//! The merged device list.
//!
//! Two sources feed it — mDNS and the manual address book — and the same
//! machine routinely appears in both, or twice over mDNS on two interfaces.
//! The registry's whole job is making that one card.
//!
//! Identity is a two-stage affair. Before a machine answers we only know an
//! address, so it is keyed by that. Once `serverinfo` gives us Sunshine's
//! `uniqueid` the entry is re-keyed and folded into any existing entry with
//! the same id.

use std::collections::{BTreeMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{
    Activity, Device, DeviceId, PairingState, Reachability, DEFAULT_HTTPS_PORT, DEFAULT_HTTP_PORT,
};
use crate::serverinfo::ServerInfo;

pub const SELF_ID: &str = "self";

fn addr_key(address: &str, port: u16) -> DeviceId {
    format!("addr:{}:{}", address.trim().to_ascii_lowercase(), port)
}

fn uid_key(unique_id: &str) -> DeviceId {
    format!("sunshine:{unique_id}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Split a user-typed address into host and optional port.
///
/// Accepts `host`, `host:port`, `[v6]`, `[v6]:port` and a bare IPv6 literal.
pub fn split_address(input: &str) -> Result<(String, Option<u16>), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a hostname or IP address.".into());
    }

    // Reject anything that looks like a URL rather than silently mangling it.
    if trimmed.contains("://") || trimmed.contains('/') {
        return Err("Enter just a hostname or IP address, without http:// or a path.".into());
    }

    if let Some(rest) = trimmed.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| "Unclosed bracket in IPv6 address.".to_string())?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(parse_port(p)?),
            None if tail.is_empty() => None,
            None => return Err("Unexpected text after the IPv6 address.".into()),
        };
        return Ok((host.to_string(), port));
    }

    // Several colons means a bare IPv6 literal, which carries no port.
    if trimmed.matches(':').count() > 1 {
        return Ok((trimmed.to_string(), None));
    }

    match trimmed.split_once(':') {
        Some((host, port)) => Ok((host.to_string(), Some(parse_port(port)?))),
        None => Ok((trimmed.to_string(), None)),
    }
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| format!("\"{raw}\" is not a valid port."))
}

/// Turn a running app's id into its name, when we know the host's apps.
///
/// `serverinfo` only reports `currentgame` as an id, so without this a card
/// mid-session can say no more than "In a session".
fn resolve_running_app(device: &mut Device) {
    let Activity::Hosting { app_id, app_name } = &mut device.activity else {
        return;
    };
    let Some(id) = app_id.as_deref() else { return };
    if let Some(app) = device.apps.iter().find(|a| a.id == id) {
        *app_name = Some(app.name.clone());
    }
}

#[derive(Debug)]
pub enum ProbeOutcome {
    Reached {
        address: String,
        info: ServerInfo,
        /// A pairing verdict from an authenticated probe, when the response
        /// carrying the detail was not itself authenticated. Wins over
        /// `info.pairing()`, which in that case can only say Unknown.
        pairing: Option<PairingState>,
    },
    Unreachable,
}

pub struct Registry {
    devices: BTreeMap<DeviceId, Device>,
    /// Every address that belongs to this machine. Sunshine running locally
    /// advertises over mDNS like any other host, so without this the machine
    /// you are sitting at shows up twice — once as "This machine" and again
    /// as a remote card.
    local_addresses: HashSet<String>,
}

impl Registry {
    pub fn new(self_name: String, local_addresses: HashSet<String>) -> Self {
        let mut this = Self {
            devices: BTreeMap::new(),
            local_addresses: local_addresses
                .into_iter()
                .map(|a| a.to_ascii_lowercase())
                .collect(),
        };
        let mut me = Device::new(SELF_ID.to_string(), self_name);
        me.is_self = true;
        // This machine is trivially reachable; its card reports host state,
        // not network state, so it is never probed.
        me.reachability = Reachability::Online { rtt_ms: 0 };
        me.activity = Activity::Idle;
        this.devices.insert(SELF_ID.to_string(), me);
        this
    }

    pub fn devices(&self) -> Vec<Device> {
        self.devices.values().cloned().collect()
    }

    /// Everything that should be polled: all remote devices with an address,
    /// as `(id, addresses, http port, tls port)`.
    pub fn probe_targets(&self) -> Vec<(DeviceId, Vec<String>, u16, u16)> {
        self.devices
            .values()
            .filter(|d| !d.is_self && !d.addresses.is_empty())
            .map(|d| (d.id.clone(), d.probe_order(), d.http_port, d.https_port))
            .collect()
    }

    /// Record the app list fetched for a device, and resolve the name of
    /// whatever it is running now.
    pub fn set_apps(&mut self, id: &str, apps: Vec<crate::applist::HostApp>) {
        if let Some(device) = self.devices.get_mut(id) {
            device.apps = apps;
            resolve_running_app(device);
        }
    }

    /// Devices we should fetch an app list for: paired, online, and without
    /// one yet. The list changes rarely, so re-fetching every tick would be
    /// a request per host per five seconds for data that almost never moves.
    pub fn applist_targets(&self) -> Vec<(DeviceId, String, u16)> {
        self.devices
            .values()
            .filter(|d| {
                !d.is_self
                    && d.apps.is_empty()
                    && d.pairing == PairingState::Paired
                    && matches!(d.reachability, Reachability::Online { .. })
            })
            .filter_map(|d| {
                d.primary_address
                    .clone()
                    .map(|address| (d.id.clone(), address, d.https_port))
            })
            .collect()
    }

    /// Insert a fully-formed device. Only used to seed mock fixtures.
    pub fn insert_prebuilt(&mut self, device: Device) {
        self.devices.insert(device.id.clone(), device);
    }

    /// True when an address belongs to this machine.
    fn is_local(&self, address: &str) -> bool {
        self.local_addresses
            .contains(&address.trim().to_ascii_lowercase())
    }

    /// Fold addresses into the self device and hand back its id.
    fn absorb_into_self(&mut self, addresses: &[String]) -> DeviceId {
        if let Some(me) = self.devices.get_mut(SELF_ID) {
            for address in addresses {
                me.add_address(address);
            }
        }
        SELF_ID.to_string()
    }

    /// Record a machine seen over mDNS.
    pub fn upsert_mdns(&mut self, name: &str, addresses: &[String], port: u16) -> DeviceId {
        if addresses.iter().any(|a| self.is_local(a)) {
            return self.absorb_into_self(addresses);
        }

        let id = self.find_by_addresses(addresses, port).unwrap_or_else(|| {
            addr_key(addresses.first().map(String::as_str).unwrap_or(name), port)
        });

        let entry = self
            .devices
            .entry(id.clone())
            .or_insert_with(|| Device::new(id.clone(), name.to_string()));

        entry.source.mdns = true;
        entry.http_port = port;
        for address in addresses {
            entry.add_address(address);
        }
        if entry.custom_name.is_none() && !name.is_empty() {
            entry.name = name.to_string();
        }
        entry.last_seen_ms = Some(now_ms());
        id
    }

    /// Record a machine the user typed in.
    pub fn upsert_manual(
        &mut self,
        address: &str,
        port: Option<u16>,
        name: Option<String>,
    ) -> DeviceId {
        let port = port.unwrap_or(DEFAULT_HTTP_PORT);
        let owned = vec![address.to_string()];

        // Typing your own address should not produce a second card for the
        // machine you are already looking at.
        if self.is_local(address) {
            return self.absorb_into_self(&owned);
        }

        let id = self
            .find_by_addresses(&owned, port)
            .unwrap_or_else(|| addr_key(address, port));

        let entry = self
            .devices
            .entry(id.clone())
            .or_insert_with(|| Device::new(id.clone(), address.to_string()));

        entry.source.manual = true;
        entry.http_port = port;
        // The TLS port sits one below the HTTP port in Sunshine's scheme; only
        // assume that when the user did not override the HTTP port.
        entry.https_port = if port == DEFAULT_HTTP_PORT {
            DEFAULT_HTTPS_PORT
        } else {
            port.saturating_sub(5)
        };
        entry.add_address(address);
        if let Some(name) = name {
            entry.custom_name = Some(name.clone());
            entry.name = name;
        }
        id
    }

    /// Drop a manual entry. A machine also seen over mDNS survives, minus its
    /// manual flag — removing a hand-typed address should not delete a
    /// machine that is sitting right there on the network.
    pub fn remove_manual(&mut self, id: &str) -> Result<(), String> {
        let Some(device) = self.devices.get_mut(id) else {
            return Err("No such machine.".into());
        };
        if device.is_self {
            return Err("This machine cannot be removed.".into());
        }
        if !device.source.manual {
            return Err("That machine was found on the network, not added by hand.".into());
        }

        if device.source.mdns {
            device.source.manual = false;
            device.custom_name = None;
        } else {
            self.devices.remove(id);
        }
        Ok(())
    }

    /// Fold a probe result into the device, re-keying it if the machine just
    /// told us its stable id.
    pub fn apply_probe(&mut self, id: &str, outcome: ProbeOutcome) -> DeviceId {
        let Some(device) = self.devices.get_mut(id) else {
            return id.to_string();
        };

        let (info, pairing_override) = match outcome {
            ProbeOutcome::Unreachable => {
                device.reachability = Reachability::Offline;
                device.activity = Activity::Unknown;
                device.pairing = PairingState::Unknown;
                return id.to_string();
            }
            ProbeOutcome::Reached {
                address,
                info,
                pairing,
            } => {
                device.reachability = Reachability::Online {
                    rtt_ms: info.rtt_ms,
                };
                device.primary_address = Some(address);
                device.last_seen_ms = Some(now_ms());
                (info, pairing)
            }
        };

        device.pairing = pairing_override.unwrap_or_else(|| info.pairing());
        device.activity = info.activity();
        device.server = Some(info.details());
        resolve_running_app(device);

        // An app list belongs to a pairing. Losing the pairing invalidates
        // it, and keeping it would offer launch buttons that cannot work.
        if device.pairing == PairingState::NotPaired {
            device.apps.clear();
        }
        if device.custom_name.is_none() {
            if let Some(hostname) = info.hostname() {
                device.name = hostname.to_string();
            }
        }

        match info.unique_id() {
            Some(unique_id) => self.rekey(id, &uid_key(unique_id)),
            None => id.to_string(),
        }
    }

    /// Move an entry to its stable key, merging with whatever is already
    /// there. This is what collapses "same machine, two addresses" into one
    /// card.
    fn rekey(&mut self, from: &str, to: &DeviceId) -> DeviceId {
        if from == to {
            return to.clone();
        }
        let Some(mut moving) = self.devices.remove(from) else {
            return to.clone();
        };
        moving.id = to.clone();

        match self.devices.get_mut(to) {
            Some(existing) => {
                existing.source.mdns |= moving.source.mdns;
                existing.source.manual |= moving.source.manual;
                for address in &moving.addresses {
                    existing.add_address(address);
                }
                // The fresher probe wins for live state.
                existing.reachability = moving.reachability;
                existing.pairing = moving.pairing;
                existing.activity = moving.activity;
                existing.server = moving.server;
                existing.last_seen_ms = moving.last_seen_ms.max(existing.last_seen_ms);
                if let Some(address) = moving.primary_address {
                    existing.primary_address = Some(address);
                }
                if existing.custom_name.is_none() {
                    if let Some(name) = moving.custom_name {
                        existing.custom_name = Some(name.clone());
                        existing.name = name;
                    } else {
                        existing.name = moving.name;
                    }
                }
            }
            None => {
                self.devices.insert(to.clone(), moving);
            }
        }
        to.clone()
    }

    /// Find an existing entry that already claims one of these addresses on
    /// the same port, so a second sighting does not create a second card.
    fn find_by_addresses(&self, addresses: &[String], port: u16) -> Option<DeviceId> {
        self.devices
            .values()
            .find(|device| {
                !device.is_self
                    && device.http_port == port
                    && device.addresses.iter().any(|known| {
                        addresses
                            .iter()
                            .any(|candidate| known.eq_ignore_ascii_case(candidate))
                    })
            })
            .map(|device| device.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn registry() -> Registry {
        Registry::new("Studio".into(), HashSet::from(["192.168.1.10".to_string()]))
    }

    fn info(fields: &[(&str, &str)]) -> ServerInfo {
        ServerInfo {
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            rtt_ms: 7,
            authenticated: false,
            status_code: crate::serverinfo::STATUS_OK,
        }
    }

    fn reached(address: &str, info: ServerInfo) -> ProbeOutcome {
        ProbeOutcome::Reached {
            address: address.into(),
            info,
            pairing: None,
        }
    }

    #[test]
    fn self_device_exists_and_is_never_probed() {
        let r = registry();
        assert_eq!(r.devices().len(), 1);
        assert!(r.probe_targets().is_empty());
    }

    #[test]
    fn the_same_address_from_both_sources_is_one_card() {
        let mut r = registry();
        r.upsert_mdns("WORKSHOP", &["192.168.1.40".into()], DEFAULT_HTTP_PORT);
        r.upsert_manual("192.168.1.40", None, Some("Workshop PC".into()));

        let remote: Vec<_> = r.devices().into_iter().filter(|d| !d.is_self).collect();
        assert_eq!(remote.len(), 1);
        assert!(remote[0].source.mdns && remote[0].source.manual);
        assert_eq!(remote[0].name, "Workshop PC");
    }

    #[test]
    fn two_addresses_collapse_once_the_machine_reports_its_id() {
        let mut r = registry();
        let lan = r.upsert_mdns("WORKSHOP", &["192.168.1.40".into()], DEFAULT_HTTP_PORT);
        let vpn = r.upsert_manual("100.84.2.9", None, None);
        assert_eq!(r.devices().len(), 3); // self + two

        let probe = |address: &str| {
            reached(
                address,
                info(&[("uniqueid", "abc-123"), ("hostname", "WORKSHOP")]),
            )
        };
        r.apply_probe(&lan, probe("192.168.1.40"));
        r.apply_probe(&vpn, probe("100.84.2.9"));

        let remote: Vec<_> = r.devices().into_iter().filter(|d| !d.is_self).collect();
        assert_eq!(remote.len(), 1, "same uniqueid must mean one card");
        assert_eq!(remote[0].addresses.len(), 2);
        assert!(remote[0].source.mdns && remote[0].source.manual);
    }

    #[test]
    fn a_user_name_survives_discovery_and_merging() {
        let mut r = registry();
        let id = r.upsert_manual("192.168.1.40", None, Some("Workshop PC".into()));
        r.apply_probe(
            &id,
            reached(
                "192.168.1.40",
                info(&[("uniqueid", "abc-123"), ("hostname", "DESKTOP-8HF2K1")]),
            ),
        );
        let device = r.devices().into_iter().find(|d| !d.is_self).unwrap();
        assert_eq!(device.name, "Workshop PC");
    }

    #[test]
    fn removing_a_manual_entry_keeps_a_machine_still_on_the_network() {
        let mut r = registry();
        r.upsert_mdns("WORKSHOP", &["192.168.1.40".into()], DEFAULT_HTTP_PORT);
        let id = r.upsert_manual("192.168.1.40", None, None);

        r.remove_manual(&id).expect("removes");
        let remote: Vec<_> = r.devices().into_iter().filter(|d| !d.is_self).collect();
        assert_eq!(remote.len(), 1);
        assert!(!remote[0].source.manual && remote[0].source.mdns);
    }

    #[test]
    fn removing_a_purely_manual_entry_deletes_it() {
        let mut r = registry();
        let id = r.upsert_manual("100.84.2.9", None, None);
        r.remove_manual(&id).expect("removes");
        assert!(r.devices().into_iter().all(|d| d.is_self));
    }

    #[test]
    fn unreachable_clears_live_state_but_keeps_the_card() {
        let mut r = registry();
        let id = r.upsert_manual("192.168.1.40", None, None);
        r.apply_probe(&id, ProbeOutcome::Unreachable);
        let device = r.devices().into_iter().find(|d| !d.is_self).unwrap();
        assert_eq!(device.reachability, Reachability::Offline);
        assert_eq!(device.activity, Activity::Unknown);
    }

    #[test]
    fn an_authenticated_verdict_beats_the_plain_probes_unknown() {
        // The detail came from the unauthenticated port, so info.pairing() can
        // only say Unknown — but a TLS 401 already told us where we stand.
        let mut r = registry();
        let id = r.upsert_manual("192.168.1.40", None, None);
        r.apply_probe(
            &id,
            ProbeOutcome::Reached {
                address: "192.168.1.40".into(),
                info: info(&[("hostname", "WORKSHOP")]),
                pairing: Some(PairingState::NotPaired),
            },
        );
        let device = r.devices().into_iter().find(|d| !d.is_self).unwrap();
        assert_eq!(device.pairing, PairingState::NotPaired);
    }

    #[test]
    fn a_local_sunshine_advertisement_does_not_become_a_second_card() {
        // Sunshine running on this machine advertises over mDNS like any
        // other host, so the grid must not show the machine twice.
        let mut r = registry();
        let id = r.upsert_mdns("Studio", &["192.168.1.10".into()], DEFAULT_HTTP_PORT);

        assert_eq!(id, SELF_ID);
        let devices = r.devices();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].is_self);
        assert!(devices[0].addresses.contains(&"192.168.1.10".to_string()));
    }

    #[test]
    fn typing_your_own_address_folds_into_this_machine() {
        let mut r = registry();
        let id = r.upsert_manual("192.168.1.10", None, Some("Me".into()));
        assert_eq!(id, SELF_ID);
        assert!(r.devices().iter().all(|d| d.is_self));
    }

    #[test]
    fn the_self_device_cannot_be_removed() {
        let mut r = registry();
        assert!(r.remove_manual(SELF_ID).is_err());
    }

    #[test]
    fn addresses_parse_into_host_and_port() {
        assert_eq!(
            split_address("192.168.1.40").unwrap(),
            ("192.168.1.40".to_string(), None)
        );
        assert_eq!(
            split_address("workshop.local:47989").unwrap(),
            ("workshop.local".to_string(), Some(47989))
        );
        assert_eq!(
            split_address("[fe80::1]:47989").unwrap(),
            ("fe80::1".to_string(), Some(47989))
        );
        assert_eq!(
            split_address("fe80::1").unwrap(),
            ("fe80::1".to_string(), None)
        );
        assert!(split_address("").is_err());
        assert!(split_address("http://192.168.1.40").is_err());
        assert!(split_address("192.168.1.40:0").is_err());
        assert!(split_address("192.168.1.40:notaport").is_err());
    }
}

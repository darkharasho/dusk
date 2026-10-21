//! The merged device list.
//!
//! Three sources feed it — mDNS, the manual address book, and the machines
//! moonlight-qt remembers — and the same machine routinely appears in more
//! than one, or twice over mDNS on two interfaces. The registry's whole job
//! is making that one card.
//!
//! Identity is a two-stage affair. Before a machine answers we only know an
//! address, so it is keyed by that. Once `serverinfo` gives us Sunshine's
//! `uniqueid` the entry is re-keyed and folded into any existing entry with
//! the same id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{
    Activity, Device, DeviceId, PairingState, Reachability, DEFAULT_HTTPS_PORT, DEFAULT_HTTP_PORT,
};
use crate::moonlight::hosts::KnownHost;
use crate::serverinfo::ServerInfo;

pub const SELF_ID: &str = "self";

fn addr_key(address: &str, port: u16) -> DeviceId {
    format!("addr:{}:{}", address.trim().to_ascii_lowercase(), port)
}

fn uid_key(unique_id: &str) -> DeviceId {
    format!("sunshine:{unique_id}")
}

/// Sunshine's TLS port sits five below its HTTP port. Only assume that when
/// the HTTP port is the stock one; a moved port could have been moved alone.
fn https_for(http_port: u16) -> u16 {
    if http_port == DEFAULT_HTTP_PORT {
        DEFAULT_HTTPS_PORT
    } else {
        http_port.saturating_sub(5)
    }
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

/// The id a machine on the account gets while nothing has found it.
///
/// Prefixed so it cannot collide with an address-keyed device, and so the
/// placeholder can be recognised and withdrawn once the real machine turns
/// up at an address.
const ACCOUNT_PREFIX: &str = "account:";

/// Names as they can be compared: machines differ on case and spacing far
/// more often than they differ on identity.
fn normalised(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn account_id(machine: &str) -> DeviceId {
    format!("{ACCOUNT_PREFIX}{machine}")
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

    /// Make the grid hold every machine on the account, found or not.
    ///
    /// Without this the grid is still a list of what is advertising: a
    /// machine of yours that is switched off, or whose hosting was
    /// uninstalled, simply vanishes -- which is what happened to Venus and
    /// reads as Dusk losing it rather than as the machine being off.
    ///
    /// `own` is this machine's own id, which is already the self card and
    /// must not appear twice.
    pub fn sync_account(&mut self, entries: &[crate::account::AccountMachine], own: Option<&str>) {
        // The account list is what decides whose machine this is, and it
        // decides it again on every tick rather than once.
        //
        // A machine is asked who it is only while it has no id (see the
        // poller), so the `mine` worked out from that one answer used to
        // stick for the life of the window. At startup the answer can easily
        // arrive before the account's list has been read, when nothing is
        // known to be yours yet -- and the machine stayed filed under "Other
        // machines on this network" until Dusk happened to be restarted at a
        // luckier moment. Re-deciding here costs a set lookup per card and
        // makes the list, not the timing, the authority.
        //
        // Only machines that have actually answered are touched. No id is
        // "never said", which is not the same as "said it is not yours", and
        // is for the name match below to deal with.
        let on_account: HashSet<&str> = entries.iter().map(|m| m.id.as_str()).collect();
        for device in self.devices.values_mut() {
            if device.is_self || device.id.starts_with(ACCOUNT_PREFIX) {
                continue;
            }
            if let Some(machine) = device.machine_id.as_deref() {
                device.mine = on_account.contains(machine);
            }
        }

        // Which ids a real, discovered device already covers. Those need no
        // placeholder, and any placeholder they had is now stale.
        let found: HashSet<String> = self
            .devices
            .values()
            .filter(|d| !d.id.starts_with(ACCOUNT_PREFIX))
            .filter_map(|d| d.machine_id.clone())
            .collect();

        // The same question answered by name, for machines that cannot yet
        // say who they are.
        //
        // The id is the right key and a name is not -- this Mac is
        // `Michaels-Mini` on the account and `Michaels-Mac-mini` over mDNS.
        // But a machine running a Dusk too old to answer `identify` has no
        // id to match on, and the alternative is what it did: one card from
        // the account with no address under "Your machines", and a second
        // from discovery under "Other machines on this network". The same
        // machine, listed twice, in two places, neither complete.
        //
        // So a name match claims the discovered card instead of adding a
        // second one. It is a weaker key, deliberately only reached when
        // there is no better one, and it stops mattering as soon as the
        // other machine is new enough to be asked.
        let by_name: HashMap<String, DeviceId> = self
            .devices
            .values()
            .filter(|d| !d.id.starts_with(ACCOUNT_PREFIX) && !d.is_self)
            .filter(|d| d.machine_id.is_none())
            .map(|d| (normalised(&d.name), d.id.clone()))
            .collect();

        let claimed: HashSet<String> = by_name.keys().cloned().collect();
        self.devices.retain(|id, _| {
            let Some(machine) = id.strip_prefix(ACCOUNT_PREFIX) else {
                return true;
            };
            // Keep only placeholders that are still on the account and still
            // unfound. A machine signed out elsewhere leaves the list, and
            // its card should go with it.
            !found.contains(machine)
                && entries.iter().any(|m| m.id == machine)
                && Some(machine) != own
                && !entries
                    .iter()
                    .any(|m| m.id == machine && claimed.contains(&normalised(&m.name)))
        });

        for entry in entries {
            let machine = &entry.id;
            if found.contains(machine) || Some(machine.as_str()) == own {
                continue;
            }

            // Claimed by name: one card, in the right half of the grid,
            // carrying the address discovery found. Its pairing state is
            // whatever the probe says, so it reads as yours and unpaired
            // rather than as a stranger.
            if let Some(id) = by_name.get(&normalised(&entry.name)) {
                if let Some(device) = self.devices.get_mut(id) {
                    device.mine = true;
                }
                self.devices.remove(&account_id(machine));
                continue;
            }
            let id = account_id(machine);
            if let Some(existing) = self.devices.get_mut(&id) {
                existing.name = entry.name.clone();
                // Addresses can change under a machine that is still off, so
                // the published set replaces rather than merges.
                existing.addresses = entry.addresses.clone();
                continue;
            }
            let mut device = Device::new(id, entry.name.clone());
            device.machine_id = Some(machine.clone());
            device.mine = true;
            // What the machine said about itself, so the grid has somewhere
            // to try when nothing on this network has found it. This is what
            // makes a machine across a VPN reachable without being typed in.
            device.addresses = entry.addresses.clone();
            // Not "offline" -- that is a verdict from a probe, and nothing
            // has been probed. This machine is yours and nothing has found
            // it, which is what the card should say.
            device.reachability = Reachability::Unknown;
            self.devices.insert(device.id.clone(), device);
        }
    }

    /// Record what a machine answered when asked which one it is.
    ///
    /// Kept even when it says it has no account: "asked and has none" is a
    /// different state from "never answered", and only the first is a
    /// settled answer about whether the machine is yours.
    pub fn set_identity(&mut self, id: &str, machine_id: Option<String>, mine: bool) {
        if let Some(device) = self.devices.get_mut(id) {
            device.machine_id = machine_id;
            device.mine = mine;
        }
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
        entry.https_port = https_for(port);
        entry.add_address(address);
        if let Some(name) = name {
            entry.custom_name = Some(name.clone());
            entry.name = name;
        }
        id
    }

    /// Record a machine moonlight-qt remembers.
    ///
    /// Unlike the other two sources this one carries Sunshine's `uniqueid`
    /// up front, so the entry can be filed under its stable key immediately
    /// instead of living at an address key until the first probe. Anything
    /// already sitting at that address is folded in on the way.
    pub fn upsert_moonlight(&mut self, host: &KnownHost) -> DeviceId {
        if host.addresses.iter().any(|a| self.is_local(a)) {
            return self.absorb_into_self(&host.addresses);
        }

        let existing = self.find_by_addresses(&host.addresses, host.port);
        let id = match host.uuid.as_deref() {
            Some(uuid) => {
                let stable = uid_key(uuid);
                match existing {
                    Some(existing) => self.rekey(&existing, &stable),
                    None => stable,
                }
            }
            None => existing.unwrap_or_else(|| addr_key(host.addresses[0].as_str(), host.port)),
        };

        let entry = self
            .devices
            .entry(id.clone())
            .or_insert_with(|| Device::new(id.clone(), host.name.clone()));

        entry.source.moonlight = true;
        entry.http_port = host.port;
        entry.https_port = https_for(host.port);
        for address in &host.addresses {
            entry.add_address(address);
        }
        if entry.custom_name.is_none() && !host.name.is_empty() {
            entry.name = host.name.clone();
        }
        // No `last_seen_ms`: remembered is not seen. Stamping it here would
        // make a machine that has been off for a month look freshly present
        // until the first probe came back and said otherwise.
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
            return Err(if device.source.moonlight && !device.source.mdns {
                "That machine was remembered from an earlier stream, not added by hand.".into()
            } else {
                "That machine was found on the network, not added by hand.".to_string()
            });
        }

        if device.source.mdns || device.source.moonlight {
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
                existing.source.moonlight |= moving.source.moonlight;
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
mod account_tests {
    use super::*;

    fn machine(id: &str, name: &str, addresses: &[&str]) -> crate::account::AccountMachine {
        crate::account::AccountMachine {
            id: id.into(),
            name: name.into(),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn entries() -> Vec<crate::account::AccountMachine> {
        vec![
            machine("aaa", "Venus", &["100.101.8.12"]),
            machine("bbb", "R-3X", &[]),
            machine("ccc", "Michaels-Mini", &[]),
        ]
    }

    fn registry() -> Registry {
        Registry::new("Michaels-Mini".into(), HashSet::new())
    }

    #[test]
    fn a_machine_of_yours_that_is_off_still_has_a_card() {
        // The point of the whole change: Venus vanished from the grid when
        // its hosting was uninstalled, though it was still one of yours.
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        let names: Vec<String> = r.devices().into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"Venus".to_string()), "{names:?}");
        assert!(names.contains(&"R-3X".to_string()), "{names:?}");
    }

    #[test]
    fn this_machine_does_not_appear_twice() {
        // It is already the self card; a second one would be the same
        // machine listed as a remote host.
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        let mine = r
            .devices()
            .into_iter()
            .filter(|d| d.name == "Michaels-Mini")
            .count();
        assert_eq!(mine, 1);
    }

    #[test]
    fn a_placeholder_gives_way_once_the_machine_is_found() {
        // Otherwise the same machine shows twice: once as the account knows
        // it and once at the address discovery found it at.
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        assert_eq!(r.devices().iter().filter(|d| d.name == "Venus").count(), 1);

        let found = r.upsert_mdns("Venus", &["192.168.50.149".into()], 47989);
        r.set_identity(&found, Some("aaa".into()), true);
        r.sync_account(&entries(), Some("ccc"));

        let venus: Vec<Device> = r
            .devices()
            .into_iter()
            .filter(|d| d.machine_id.as_deref() == Some("aaa"))
            .collect();
        assert_eq!(venus.len(), 1, "one card for one machine");
        assert!(
            !venus[0].id.starts_with(ACCOUNT_PREFIX),
            "the real one wins"
        );
    }

    #[test]
    fn a_published_address_makes_an_unfound_machine_reachable() {
        // The whole point: from another network mDNS finds nothing, so the
        // only address the grid has is the one the machine published. If it
        // does not reach probe_targets, the card is decorative.
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        let targets = r.probe_targets();
        let venus = targets
            .iter()
            .find(|(id, ..)| id == &account_id("aaa"))
            .expect("a machine with a published address is probed");
        assert_eq!(venus.1, vec!["100.101.8.12".to_string()]);

        // And one that published nothing is not probed, rather than probed
        // with an empty list.
        assert!(!targets.iter().any(|(id, ..)| id == &account_id("bbb")));
    }

    #[test]
    fn addresses_follow_the_account_rather_than_accumulating() {
        // A machine that moves networks republishes; the stale address must
        // not linger, or the grid keeps trying somewhere it no longer is.
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        let moved = vec![
            machine("aaa", "Venus", &["100.101.8.99"]),
            machine("bbb", "R-3X", &[]),
            machine("ccc", "Michaels-Mini", &[]),
        ];
        r.sync_account(&moved, Some("ccc"));
        let venus = r
            .devices()
            .into_iter()
            .find(|d| d.machine_id.as_deref() == Some("aaa"))
            .expect("still listed");
        assert_eq!(venus.addresses, vec!["100.101.8.99".to_string()]);
    }

    #[test]
    fn a_machine_identified_before_the_account_was_read_is_still_filed_as_yours() {
        // The startup race, which this got wrong for real. A machine is
        // asked who it is only while it has no id, so the answer arrives
        // once; if the account's list has not been read by then, nothing is
        // known to be yours and the machine is filed as a stranger — and,
        // never being asked again, stays one. Observed on a real grid: the
        // only machine in the right half was the one that was *off*, because
        // an undiscovered machine comes from the placeholder path, which
        // re-decides every tick.
        let mut r = registry();
        let found = r.upsert_mdns("Venus", &["192.168.50.149".into()], 47989);

        // It answered before the list arrived, so it was filed as not yours.
        r.set_identity(&found, Some("aaa".into()), false);

        // The list arrives on a later tick.
        r.sync_account(&entries(), Some("ccc"));

        let venus = r
            .devices()
            .into_iter()
            .find(|d| d.id == found)
            .expect("still listed");
        assert!(venus.mine, "the account list decides, not the timing");

        // And the reverse still holds: a machine that says who it is and is
        // not on the account must not be claimed by this.
        let stranger = r.upsert_mdns("someone-else", &["192.168.50.200".into()], 47989);
        r.set_identity(&stranger, Some("zzz".into()), true);
        r.sync_account(&entries(), Some("ccc"));
        let stranger = r
            .devices()
            .into_iter()
            .find(|d| d.id == stranger)
            .expect("still listed");
        assert!(!stranger.mine, "not on the account, not yours");
    }

    #[test]
    fn a_machine_too_old_to_identify_is_claimed_by_name_not_duplicated() {
        // What this looked like: the same machine twice, once from the
        // account with no address under "Your machines" and once from
        // discovery under "Other machines on this network". Neither card
        // complete, and the person's own machine filed as a stranger's.
        let mut r = registry();
        let found = r.upsert_mdns("Venus", &["192.168.50.149".into()], 47989);
        // No identity: a Dusk too old to answer `identify` leaves it unset.
        r.sync_account(&entries(), Some("ccc"));

        let venus: Vec<Device> = r
            .devices()
            .into_iter()
            .filter(|d| d.name.eq_ignore_ascii_case("venus"))
            .collect();
        assert_eq!(venus.len(), 1, "one machine, one card");
        assert_eq!(venus[0].id, found, "the discovered card is the one kept");
        assert!(venus[0].mine, "and it is filed as yours");
        assert!(!venus[0].addresses.is_empty(), "keeping its address");
    }

    #[test]
    fn a_name_match_gives_way_to_a_real_identity() {
        // The name is a fallback. Once the machine can say who it is, the
        // id decides, and a card claimed by name must not block that.
        let mut r = registry();
        let found = r.upsert_mdns("Venus", &["192.168.50.149".into()], 47989);
        r.sync_account(&entries(), Some("ccc"));
        r.set_identity(&found, Some("aaa".into()), true);
        r.sync_account(&entries(), Some("ccc"));

        let venus: Vec<Device> = r
            .devices()
            .into_iter()
            .filter(|d| d.machine_id.as_deref() == Some("aaa") || d.name == "Venus")
            .collect();
        assert_eq!(venus.len(), 1);
        assert_eq!(venus[0].machine_id.as_deref(), Some("aaa"));
    }

    #[test]
    fn a_machine_taken_off_the_account_loses_its_card() {
        let mut r = registry();
        r.sync_account(&entries(), Some("ccc"));
        let fewer: Vec<crate::account::AccountMachine> =
            entries().into_iter().filter(|m| m.id != "bbb").collect();
        r.sync_account(&fewer, Some("ccc"));
        let names: Vec<String> = r.devices().into_iter().map(|d| d.name).collect();
        assert!(!names.contains(&"R-3X".to_string()), "{names:?}");
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

    fn known(name: &str, address: &str, uuid: Option<&str>) -> KnownHost {
        KnownHost {
            name: name.into(),
            addresses: vec![address.into()],
            port: DEFAULT_HTTP_PORT,
            uuid: uuid.map(str::to_string),
        }
    }

    #[test]
    fn a_machine_moonlight_remembers_gets_a_card_without_being_reachable() {
        // The point of the source: mDNS never saw this host and it may be
        // switched off, but it is still one of your machines.
        let mut r = registry();
        r.upsert_moonlight(&known("bazzite", "192.168.1.40", Some("abc-123")));

        let device = r.devices().into_iter().find(|d| !d.is_self).unwrap();
        assert_eq!(device.name, "bazzite");
        assert!(device.source.moonlight);
        assert_eq!(device.reachability, Reachability::Unknown);
        assert_eq!(
            device.last_seen_ms, None,
            "remembered is not seen; a stamp here would claim presence"
        );
    }

    #[test]
    fn a_remembered_host_is_already_on_the_key_its_probe_will_use() {
        // Moonlight's uuid is Sunshine's uniqueid, so the probe must land on
        // the existing entry rather than minting a second card.
        let mut r = registry();
        let id = r.upsert_moonlight(&known("bazzite", "192.168.1.40", Some("abc-123")));
        let probed = r.apply_probe(
            &id,
            reached(
                "192.168.1.40",
                info(&[("uniqueid", "abc-123"), ("hostname", "bazzite")]),
            ),
        );

        assert_eq!(probed, id);
        assert_eq!(r.devices().into_iter().filter(|d| !d.is_self).count(), 1);
    }

    #[test]
    fn mdns_and_moonlight_finding_the_same_machine_is_one_card() {
        let mut r = registry();
        r.upsert_mdns("BAZZITE", &["192.168.1.40".into()], DEFAULT_HTTP_PORT);
        r.upsert_moonlight(&known("bazzite", "192.168.1.40", Some("abc-123")));

        let remote: Vec<_> = r.devices().into_iter().filter(|d| !d.is_self).collect();
        assert_eq!(remote.len(), 1);
        assert!(remote[0].source.mdns && remote[0].source.moonlight);
        // Folded onto the stable key, not left at the address key.
        assert_eq!(remote[0].id, uid_key("abc-123"));
    }

    #[test]
    fn a_remembered_host_without_a_uuid_still_merges_by_address() {
        let mut r = registry();
        r.upsert_mdns("BAZZITE", &["192.168.1.40".into()], DEFAULT_HTTP_PORT);
        r.upsert_moonlight(&known("bazzite", "192.168.1.40", None));
        assert_eq!(r.devices().into_iter().filter(|d| !d.is_self).count(), 1);
    }

    #[test]
    fn moonlights_entry_for_this_machine_folds_into_self() {
        // Moonlight remembers the machine you are sitting at, usually as
        // 127.0.0.1, and that must not become a second card.
        let mut r = registry();
        let id = r.upsert_moonlight(&known("Studio", "192.168.1.10", Some("abc-123")));
        assert_eq!(id, SELF_ID);
        assert!(r.devices().iter().all(|d| d.is_self));
    }

    #[test]
    fn removing_a_manual_entry_keeps_a_machine_moonlight_remembers() {
        let mut r = registry();
        r.upsert_moonlight(&known("bazzite", "192.168.1.40", Some("abc-123")));
        let id = r.upsert_manual("192.168.1.40", None, None);

        r.remove_manual(&id).expect("removes");
        let remote: Vec<_> = r.devices().into_iter().filter(|d| !d.is_self).collect();
        assert_eq!(remote.len(), 1);
        assert!(!remote[0].source.manual && remote[0].source.moonlight);
    }

    #[test]
    fn a_purely_remembered_machine_cannot_be_removed_by_hand() {
        let mut r = registry();
        let id = r.upsert_moonlight(&known("bazzite", "192.168.1.40", Some("abc-123")));
        let err = r.remove_manual(&id).expect_err("not a manual entry");
        assert!(err.contains("earlier stream"));
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

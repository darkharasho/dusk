//! The machines moonlight-qt remembers.
//!
//! # Why this is a discovery source at all
//!
//! mDNS only finds what is advertising right now, and Sunshine hosts are
//! routinely not: the service may be off, the box asleep, or — measured on
//! the network this was written against — simply reachable and answering
//! `serverinfo` while publishing nothing over multicast. Moonlight shows
//! those machines anyway because it writes every host it has ever paired
//! with into its own store and paints a card whether or not it can be
//! reached.
//!
//! Dusk already opens that store for the client identity (see `identity`),
//! and the one field that matters most is free: `uuid` is byte-for-byte
//! Sunshine's `uniqueid`, so a remembered host arrives already keyed the way
//! the registry keys a probed one. No address heuristics, no duplicate card.
//!
//! Read-only, like `identity`. moonlight-qt owns this list; Dusk mirrors it.

use std::collections::BTreeMap;

use crate::model::DEFAULT_HTTP_PORT;

/// A machine moonlight-qt has in its list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHost {
    pub name: String,
    /// LAN address first, because that is the one worth trying first.
    pub addresses: Vec<String>,
    pub port: u16,
    /// Sunshine's `uniqueid`. Present for anything Moonlight has reached.
    pub uuid: Option<String>,
}

/// Read the host list, or an empty list where there is nothing to read.
///
/// An absent or unparseable store is an ordinary state — moonlight-qt may
/// not be installed — so this never errors. The grid simply falls back to
/// mDNS and the address book.
pub fn load() -> Vec<KnownHost> {
    assemble(&platform::fields())
}

/// The per-host fields, flattened to `"<index>.<field>"`.
///
/// Every backend normalises to this shape so the interesting half — which
/// addresses to keep and which to throw away — is one function that can be
/// tested on any platform. Without that, the Windows and Linux paths would
/// only ever be exercised by running Windows and Linux.
type Fields = BTreeMap<String, String>;

/// Build the host list from flattened fields.
fn assemble(fields: &Fields) -> Vec<KnownHost> {
    let mut by_index: BTreeMap<u32, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in fields {
        let Some((index, field)) = key.split_once('.') else {
            continue;
        };
        // `hosts.size` and anything else non-numeric is not a host.
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        by_index
            .entry(index)
            .or_default()
            .insert(field, value.as_str());
    }

    by_index
        .values()
        .filter_map(|host| {
            let get = |name: &str| host.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());

            // Order is probe order: the registry makes the first address the
            // primary one, and on a LAN or a VPN that should be the local
            // address rather than a hostname that may not resolve.
            let mut addresses = Vec::new();
            for field in ["localaddress", "manualaddress", "ipv6address"] {
                if let Some(address) = get(field) {
                    let address = address.to_string();
                    if !addresses.contains(&address) {
                        addresses.push(address);
                    }
                }
            }
            // `remoteaddress` is deliberately dropped. It is the host's
            // public IP, which every machine behind one router shares — so
            // keeping it would make the registry's address matching fold
            // four unrelated hosts into a single card. Reaching a machine
            // over the open internet is out of scope anyway.
            if addresses.is_empty() {
                return None;
            }

            let name = get("hostname").unwrap_or(&addresses[0]).to_string();
            let port = get("localport")
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|p| *p > 0)
                .unwrap_or(DEFAULT_HTTP_PORT);

            Some(KnownHost {
                name,
                addresses,
                port,
                uuid: get("uuid").map(str::to_string),
            })
        })
        .collect()
}

// ------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use super::Fields;
    use std::path::PathBuf;

    fn store_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Preferences/com.moonlight-stream.Moonlight.plist"))
    }

    /// CFPreferences has no nesting, so QSettings writes an array flat:
    /// `hosts.1.hostname`, `hosts.size`, and so on.
    pub fn fields() -> Fields {
        let Some(path) = store_path() else {
            return Fields::new();
        };
        let Ok(value) = plist::Value::from_file(path) else {
            return Fields::new();
        };
        let Some(dict) = value.into_dictionary() else {
            return Fields::new();
        };

        dict.into_iter()
            .filter_map(|(key, value)| {
                let rest = key.strip_prefix("hosts.")?;
                Some((rest.to_string(), scalar(&value)?))
            })
            .collect()
    }

    /// Ports come back as integers and everything else as strings.
    fn scalar(value: &plist::Value) -> Option<String> {
        match value {
            plist::Value::String(s) => Some(s.clone()),
            plist::Value::Integer(i) => Some(i.to_string()),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
mod platform {
    use super::Fields;

    /// The first store that remembers any host wins. An empty `[hosts]` is
    /// indistinguishable from an absent one here and both are worth skipping,
    /// so a stale native config cannot shadow a Flatpak one in use.
    pub fn fields() -> Fields {
        crate::moonlight::linux_store_paths()
            .into_iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .map(|text| super::ini_host_fields(&text))
            .find(|fields| !fields.is_empty())
            .unwrap_or_default()
    }
}

// ----------------------------------------------------------------- Windows

#[cfg(target_os = "windows")]
mod platform {
    use super::Fields;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const SUBKEY: &str = r"Software\Moonlight Game Streaming Project\Moonlight\hosts";

    /// The registry backend nests, so the array is a subkey per index.
    pub fn fields() -> Fields {
        let Ok(hosts) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(SUBKEY) else {
            return Fields::new();
        };

        let mut out = Fields::new();
        for index in hosts.enum_keys().flatten() {
            let Ok(host) = hosts.open_subkey(&index) else {
                continue;
            };
            for (name, _) in host.enum_values().flatten() {
                if let Some(value) = scalar(&host, &name) {
                    out.insert(format!("{index}.{name}"), value);
                }
            }
        }
        out
    }

    /// Ports are written as DWORDs and everything else as strings.
    fn scalar(key: &RegKey, name: &str) -> Option<String> {
        if let Ok(text) = key.get_value::<String, _>(name) {
            return Some(text);
        }
        key.get_value::<u32, _>(name).ok().map(|n| n.to_string())
    }
}

/// Pull the `[hosts]` group out of a QSettings INI file.
///
/// QSettings writes an array as a group whose keys carry the index in front
/// of a backslash — `1\hostname=bazzite` — alongside a `size` key that is
/// not a host. Section tracking is required rather than optional: `1\name`
/// is a plausible key in more than one group.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ini_host_fields(text: &str) -> Fields {
    let mut out = Fields::new();
    let mut in_hosts = false;

    for line in text.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_hosts = section.eq_ignore_ascii_case("hosts");
            continue;
        }
        if !in_hosts || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        out.insert(
            key.trim().replace('\\', "."),
            crate::moonlight::unquote(value.trim()).to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_remembered_host_becomes_a_known_host() {
        let hosts = assemble(&fields(&[
            ("size", "1"),
            ("1.hostname", "bazzite"),
            ("1.localaddress", "192.168.50.146"),
            ("1.localport", "47989"),
            ("1.uuid", "4F870966-1A5C-DF0E-1A0A-B711ED48CD13"),
        ]));

        assert_eq!(
            hosts,
            vec![KnownHost {
                name: "bazzite".into(),
                addresses: vec!["192.168.50.146".into()],
                port: 47989,
                uuid: Some("4F870966-1A5C-DF0E-1A0A-B711ED48CD13".into()),
            }]
        );
    }

    #[test]
    fn the_public_address_is_dropped() {
        // Every machine behind one router reports the same `remoteaddress`,
        // so keeping it would collapse them all into a single card.
        let hosts = assemble(&fields(&[
            ("1.hostname", "bazzite"),
            ("1.localaddress", "192.168.50.146"),
            ("1.remoteaddress", "47.144.41.231"),
            ("2.hostname", "picomp"),
            ("2.localaddress", "192.168.50.69"),
            ("2.remoteaddress", "47.144.41.231"),
        ]));

        assert_eq!(hosts.len(), 2);
        for host in hosts {
            assert!(!host.addresses.contains(&"47.144.41.231".to_string()));
        }
    }

    #[test]
    fn the_local_address_is_tried_first() {
        let hosts = assemble(&fields(&[
            ("1.hostname", "workshop"),
            ("1.manualaddress", "100.84.2.9"),
            ("1.localaddress", "192.168.50.146"),
            ("1.ipv6address", "fdbb::1"),
        ]));
        assert_eq!(
            hosts[0].addresses,
            vec!["192.168.50.146", "100.84.2.9", "fdbb::1"]
        );
    }

    #[test]
    fn a_repeated_address_is_kept_once() {
        // Moonlight writes the same string to `localaddress` and
        // `manualaddress` for a host that was added by hand and then found.
        let hosts = assemble(&fields(&[
            ("1.hostname", "bazzite"),
            ("1.localaddress", "192.168.50.146"),
            ("1.manualaddress", "192.168.50.146"),
        ]));
        assert_eq!(hosts[0].addresses, vec!["192.168.50.146"]);
    }

    #[test]
    fn a_host_with_no_usable_address_is_skipped() {
        let hosts = assemble(&fields(&[
            ("1.hostname", "ghost"),
            ("1.localaddress", ""),
            ("1.manualaddress", ""),
            ("1.remoteaddress", "47.144.41.231"),
        ]));
        assert!(hosts.is_empty());
    }

    #[test]
    fn the_size_key_is_not_mistaken_for_a_host() {
        let hosts = assemble(&fields(&[("size", "4"), ("notanindex.x", "y")]));
        assert!(hosts.is_empty());
    }

    #[test]
    fn a_missing_or_zero_port_falls_back_to_sunshines_default() {
        let hosts = assemble(&fields(&[
            ("1.localaddress", "192.168.50.146"),
            ("2.localaddress", "192.168.50.69"),
            ("2.localport", "0"),
        ]));
        assert_eq!(hosts[0].port, DEFAULT_HTTP_PORT);
        assert_eq!(hosts[1].port, DEFAULT_HTTP_PORT);
    }

    #[test]
    fn a_nameless_host_is_labelled_by_address() {
        let hosts = assemble(&fields(&[("1.localaddress", "192.168.50.146")]));
        assert_eq!(hosts[0].name, "192.168.50.146");
    }

    #[test]
    fn hosts_come_back_in_index_order() {
        let hosts = assemble(&fields(&[
            ("10.localaddress", "192.168.50.10"),
            ("2.localaddress", "192.168.50.2"),
        ]));
        // Numeric, not lexical: "10" must not sort before "2".
        assert_eq!(hosts[0].addresses[0], "192.168.50.2");
    }

    #[test]
    fn the_ini_backend_reads_only_the_hosts_group() {
        let ini = "[General]\n1\\hostname=notahost\nfps=60\n\
                   [hosts]\n1\\hostname=bazzite\n1\\localaddress=192.168.50.146\nsize=1\n\
                   [gcmapping]\n1\\hostname=alsonotahost\n";
        let hosts = assemble(&ini_host_fields(ini));
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "bazzite");
    }

    #[test]
    fn the_ini_backend_survives_a_file_with_no_hosts() {
        assert!(assemble(&ini_host_fields("[General]\nfps=60\n")).is_empty());
    }
}

/// Reads the real moonlight-qt store on this machine. Ignored because it can
/// only say anything on a machine that has one — but on a machine that does,
/// it is the only check that the path, the INI parse and the quoting all
/// agree with what moonlight-qt actually wrote.
#[cfg(all(test, target_os = "linux"))]
mod live {
    #[test]
    #[ignore]
    fn the_local_store_is_found_and_read() {
        for path in crate::moonlight::linux_store_paths() {
            println!("candidate: {} exists={}", path.display(), path.exists());
        }

        let hosts = super::load();
        println!("remembered hosts: {}", hosts.len());
        for host in &hosts {
            println!("  {host:?}");
        }
        println!("identity: {:?}", crate::moonlight::identity::load());

        assert!(
            !hosts.is_empty(),
            "no remembered hosts: moonlight-qt has never connected to one, or the store was not found"
        );
        assert!(
            crate::moonlight::identity::load().is_some(),
            "no client identity: moonlight-qt has never paired, or the store was not found"
        );
    }
}

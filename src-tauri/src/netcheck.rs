//! Whether this machine has two ways onto the same network.
//!
//! A Mac with Ethernet plugged in and Wi-Fi left on, both given an address
//! on the same LAN, is dual-homed on one segment. The routing table then
//! carries two default routes and two paths to the subnet, every host gets
//! cached against both interfaces, and which one a given connection leaves
//! by is not a thing anybody decided. Measured on this Mac on 2026-11-11:
//!
//! ```text
//! default        192.168.50.1   UGScg    en0
//! default        192.168.50.1   UGScIg   en1
//! 192.168.50     link#7         UCS      en0 !
//! 192.168.50     link#15        UCSI     en1 !
//! ```
//!
//! The symptom is not a machine that is unreachable. It is a machine that is
//! reachable, then briefly is not, then is again — twice that day, for about
//! a minute each time, with every other machine on the LAN going with it and
//! `ping` still working while TCP would not.
//!
//! # What this can and cannot do
//!
//! It cannot fix it. The stream is carried by a separate process with its own
//! sockets, and there is no flag to tell it which interface to leave by; the
//! honest fix is on the machine, by turning one of the two off.
//!
//! What it can do is stop the condition being invisible. The cost of this on
//! the day was not the minute of no network, it was that nothing anywhere
//! connected a dead stream to a second interface nobody was thinking about.
//! So this is named at startup and again in the one message where it is most
//! likely to be the answer: a machine that was there a moment ago and has
//! stopped answering now.

use std::net::Ipv4Addr;

/// Two interfaces holding addresses on the same network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedSubnet {
    /// The interfaces, in the order the system listed them.
    pub first: String,
    pub second: String,
    /// The network they share, for the message.
    pub network: Ipv4Addr,
}

impl SharedSubnet {
    /// A sentence for a person, naming the two and what it does.
    pub fn sentence(&self) -> String {
        format!(
            "This machine is on {} through both {} and {}, \
             which can make machines appear to come and go. \
             Turning one of the two off is the fix.",
            self.network, self.first, self.second
        )
    }
}

fn network_of(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) & u32::from(mask))
}

/// Whether an address is one worth comparing.
///
/// Link-local is what an interface has when it has nothing: two of those
/// share `169.254.0.0/16` without sharing a network in any sense that
/// matters. A `/32` — which is what Tailscale hands out — shares a network
/// with nothing by definition, and is skipped rather than special-cased by
/// name, because the next overlay network will not be called Tailscale.
fn worth_comparing(ip: Ipv4Addr, mask: Ipv4Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_link_local()
        && u32::from(mask) != u32::MAX
        && !mask.is_unspecified()
}

/// The first pair of distinct interfaces sharing a network, from a list of
/// `(interface, address, netmask)`.
///
/// Split from [`found`] so the decision can be tested against addresses that
/// were captured rather than invented.
fn shared(interfaces: &[(String, Ipv4Addr, Ipv4Addr)]) -> Option<SharedSubnet> {
    let usable: Vec<_> = interfaces
        .iter()
        .filter(|(_, ip, mask)| worth_comparing(*ip, *mask))
        .collect();

    for (i, (name, ip, mask)) in usable.iter().enumerate() {
        for (other, other_ip, other_mask) in usable.iter().skip(i + 1) {
            // Same name is one interface with two addresses on it, which is
            // an alias and not a second way in.
            if name == other || mask != other_mask {
                continue;
            }
            if network_of(*ip, *mask) == network_of(*other_ip, *other_mask) {
                return Some(SharedSubnet {
                    first: (*name).clone(),
                    second: (*other).clone(),
                    network: network_of(*ip, *mask),
                });
            }
        }
    }
    None
}

/// Ask the system, and decide. `None` when it cannot be asked, which reads
/// the same as nothing found: this only ever adds a sentence.
pub fn found() -> Option<SharedSubnet> {
    let interfaces = if_addrs::get_if_addrs().ok()?;
    let v4: Vec<(String, Ipv4Addr, Ipv4Addr)> = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) => Some((interface.name, v4.ip, v4.netmask)),
            _ => None,
        })
        .collect();
    shared(&v4)
}

/// Say it once, at startup, so a log from a machine in this state carries
/// that fact above everything it explains.
pub fn say_once() {
    if let Some(shared) = found() {
        eprintln!("dusk: {}", shared.sentence());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, ip: &str, mask: &str) -> (String, Ipv4Addr, Ipv4Addr) {
        (name.into(), ip.parse().unwrap(), mask.parse().unwrap())
    }

    #[test]
    fn ethernet_and_wifi_on_one_lan_are_found() {
        // Captured from this Mac on 2026-11-11, where it had been the
        // unexplained half of two dead streams.
        let found = shared(&[
            iface("lo0", "127.0.0.1", "255.0.0.0"),
            iface("en0", "192.168.50.34", "255.255.255.0"),
            iface("en1", "192.168.50.33", "255.255.255.0"),
        ])
        .expect("two ways onto one network");

        assert_eq!(found.first, "en0");
        assert_eq!(found.second, "en1");
        assert_eq!(found.network, Ipv4Addr::new(192, 168, 50, 0));
    }

    #[test]
    fn one_way_in_is_nothing_to_say() {
        assert_eq!(
            shared(&[
                iface("lo0", "127.0.0.1", "255.0.0.0"),
                iface("en0", "192.168.50.34", "255.255.255.0"),
            ]),
            None
        );
    }

    #[test]
    fn overlay_networks_are_not_a_second_way_onto_the_lan() {
        // The same machine also runs Tailscale and ZeroTier. Neither shares
        // the LAN's network, and the Tailscale address is a /32, which
        // shares a network with nothing at all.
        assert_eq!(
            shared(&[
                iface("en0", "192.168.50.34", "255.255.255.0"),
                iface("utun4", "100.120.26.79", "255.255.255.255"),
                iface("ztyqbr7h6j", "10.147.19.148", "255.255.255.0"),
            ]),
            None
        );
    }

    #[test]
    fn two_addresses_on_one_interface_are_an_alias() {
        assert_eq!(
            shared(&[
                iface("en0", "192.168.50.34", "255.255.255.0"),
                iface("en0", "192.168.50.35", "255.255.255.0"),
            ]),
            None
        );
    }

    #[test]
    fn an_interface_with_nothing_shares_nothing() {
        // Two interfaces that failed to get an address both hold a
        // link-local one. They are not a second way onto anything.
        assert_eq!(
            shared(&[
                iface("en5", "169.254.1.2", "255.255.0.0"),
                iface("en6", "169.254.9.9", "255.255.0.0"),
            ]),
            None
        );
    }

    #[test]
    fn the_same_network_through_different_masks_is_not_compared() {
        // A /24 and a /16 that happen to overlap are a different situation
        // and not the one this is about; said plainly rather than guessed at.
        assert_eq!(
            shared(&[
                iface("en0", "192.168.50.34", "255.255.255.0"),
                iface("en1", "192.168.50.33", "255.255.0.0"),
            ]),
            None
        );
    }
}
